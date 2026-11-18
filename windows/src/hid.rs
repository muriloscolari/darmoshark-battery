// SPDX-License-Identifier: GPL-2.0-or-later
//! Windows HID enumeration and Darmoshark battery queries via `windows-sys`.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetFeature, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidD_SetFeature, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS,
    HIDP_STATUS_SUCCESS, PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::protocol::{
    build_nordic_power, build_telink_feature, identify_device, is_nordic_power_response,
    is_telink_echo, parse_nordic_power, parse_telink_power, DeviceSpec, Transport, DS_REPORT_LEN,
    DS_TELINK_OP_BOND, DS_TELINK_OP_IDENTIFY, DS_TELINK_REPORT_ID, DS_TELINK_REPORT_LEN,
};

const READ_TIMEOUT_MS: u32 = 1500;
const PRIMER_TIMEOUT_MS: u64 = 800;
const TELINK_TIMEOUT_MS: u64 = 1500;
const TELINK_POLL_STEP_MS: u64 = 15;

#[derive(Debug, Clone)]
pub struct BatteryReading {
    pub model: &'static str,
    pub percent: Option<u8>,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct DeviceInfoSet(HDEVINFO);

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        if self.0 != 0 && self.0 != -1 {
            unsafe {
                SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }
}

struct PreparsedData(PHIDP_PREPARSED_DATA);

impl Drop for PreparsedData {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                HidD_FreePreparsedData(self.0);
            }
        }
    }
}

/// Find the first supported Darmoshark configuration HID interface and query its battery.
pub fn poll_battery() -> Result<Option<BatteryReading>, String> {
    let mut hid_guid = unsafe { zeroed() };
    unsafe { HidD_GetHidGuid(&mut hid_guid) };

    let dev_info = unsafe {
        SetupDiGetClassDevsW(
            &hid_guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if dev_info == -1 || dev_info == 0 {
        return Err(format!(
            "SetupDiGetClassDevsW failed ({})",
            unsafe { GetLastError() }
        ));
    }
    let _dev_info_guard = DeviceInfoSet(dev_info);

    let mut index: u32 = 0;
    loop {
        let mut iface_data: SP_DEVICE_INTERFACE_DATA = unsafe { zeroed() };
        iface_data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(dev_info, null(), &hid_guid, index, &mut iface_data)
        };
        index += 1;
        if ok == 0 {
            break;
        }

        let Some(path) = get_interface_path(dev_info, &mut iface_data) else {
            continue;
        };

        let Some((spec, caps)) = probe_interface(&path) else {
            continue;
        };

        // Re-open with read/write access for HID I/O.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(format!(
                "Cannot open {} ({})",
                spec.model,
                unsafe { GetLastError() }
            ));
        }
        let _handle_guard = OwnedHandle(handle);

        let percent = match spec.transport {
            Transport::Nordic => query_nordic(handle, &caps, spec.wireless)?,
            Transport::Telink => query_telink(handle, &caps, spec.wireless)?,
        };

        return Ok(Some(BatteryReading {
            model: spec.model,
            percent,
        }));
    }

    Ok(None)
}

fn get_interface_path(
    dev_info: HDEVINFO,
    iface_data: &mut SP_DEVICE_INTERFACE_DATA,
) -> Option<Vec<u16>> {
    let mut required_bytes: u32 = 0;
    unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            dev_info,
            iface_data,
            null_mut(),
            0,
            &mut required_bytes,
            null_mut(),
        );
    }
    if required_bytes < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
        return None;
    }

    let mut buf = vec![0u8; required_bytes as usize];
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    unsafe {
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
    }

    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            dev_info,
            iface_data,
            detail,
            required_bytes,
            null_mut(),
            null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }

    let path_offset = unsafe {
        (&(*detail).DevicePath as *const u16 as usize) - (detail as usize)
    };
    let path_bytes = &buf[path_offset..];
    let wide: Vec<u16> = path_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&w| w != 0)
        .chain(std::iter::once(0))
        .collect();
    Some(wide)
}

fn probe_interface(path: &[u16]) -> Option<(DeviceSpec, HIDP_CAPS)> {
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }
    let _guard = OwnedHandle(handle);

    let mut attrs: HIDD_ATTRIBUTES = unsafe { zeroed() };
    attrs.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
    if unsafe { !HidD_GetAttributes(handle, &mut attrs) } {
        return None;
    }

    let spec = identify_device(attrs.VendorID, attrs.ProductID)?;

    let mut preparsed: PHIDP_PREPARSED_DATA = 0;
    if unsafe { !HidD_GetPreparsedData(handle, &mut preparsed) } || preparsed == 0 {
        return None;
    }
    let _preparsed_guard = PreparsedData(preparsed);

    let mut caps: HIDP_CAPS = unsafe { zeroed() };
    if unsafe { HidP_GetCaps(preparsed, &mut caps) } != HIDP_STATUS_SUCCESS {
        return None;
    }

    if caps.UsagePage == spec.usage_page() && caps.Usage == spec.usage() {
        Some((spec, caps))
    } else {
        None
    }
}

fn write_overlapped(handle: HANDLE, data: &[u8]) -> Result<(), String> {
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() {
        return Err("CreateEventW failed".to_string());
    }
    let _event_guard = OwnedHandle(event);

    let mut ov: OVERLAPPED = unsafe { zeroed() };
    ov.hEvent = event;
    let mut written: u32 = 0;

    let ok = unsafe {
        WriteFile(
            handle,
            data.as_ptr(),
            data.len() as u32,
            &mut written,
            &mut ov,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err != ERROR_IO_PENDING {
            return Err(format!("WriteFile failed ({err})"));
        }
        let wait = unsafe { WaitForSingleObject(event, READ_TIMEOUT_MS) };
        if wait != WAIT_OBJECT_0 {
            unsafe { CancelIoEx(handle, &ov) };
            return Err("WriteFile timed out".to_string());
        }
        if unsafe { GetOverlappedResult(handle, &ov, &mut written, 0) } == 0 {
            return Err(format!(
                "WriteFile GetOverlappedResult failed ({})",
                unsafe { GetLastError() }
            ));
        }
    }
    Ok(())
}

fn read_overlapped(handle: HANDLE, buf: &mut [u8], timeout_ms: u32) -> Result<usize, String> {
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() {
        return Err("CreateEventW failed".to_string());
    }
    let _event_guard = OwnedHandle(event);

    let mut ov: OVERLAPPED = unsafe { zeroed() };
    ov.hEvent = event;
    let mut read_bytes: u32 = 0;

    let ok = unsafe {
        ReadFile(
            handle,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut read_bytes,
            &mut ov,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err != ERROR_IO_PENDING {
            return Err(format!("ReadFile failed ({err})"));
        }
        let wait = unsafe { WaitForSingleObject(event, timeout_ms) };
        if wait != WAIT_OBJECT_0 {
            unsafe { CancelIoEx(handle, &ov) };
            // Drain cancellation result so the stack OVERLAPPED is no longer referenced.
            let mut dummy: u32 = 0;
            unsafe { GetOverlappedResult(handle, &ov, &mut dummy, 1) };
            return Err("timeout".to_string());
        }
        if unsafe { GetOverlappedResult(handle, &ov, &mut read_bytes, 0) } == 0 {
            return Err(format!(
                "ReadFile GetOverlappedResult failed ({})",
                unsafe { GetLastError() }
            ));
        }
    }
    Ok(read_bytes as usize)
}

fn query_nordic(handle: HANDLE, caps: &HIDP_CAPS, wireless: bool) -> Result<Option<u8>, String> {
    // Windows HID prepends 1 byte for Report ID (0x00 for non-numbered reports).
    let out_len = (caps.OutputReportByteLength as usize).max(DS_REPORT_LEN + 1);
    let in_len = (caps.InputReportByteLength as usize).max(DS_REPORT_LEN + 1);

    let cmd = build_nordic_power(wireless);
    let mut out_buf = vec![0u8; out_len];
    out_buf[0] = 0x00;
    out_buf[1..1 + DS_REPORT_LEN].copy_from_slice(&cmd);

    write_overlapped(handle, &out_buf)?;

    let mut in_buf = vec![0u8; in_len];
    for _ in 0..8 {
        match read_overlapped(handle, &mut in_buf, READ_TIMEOUT_MS) {
            Ok(n) if n > 1 => {
                let payload = &in_buf[1..n];
                if !is_nordic_power_response(payload) {
                    continue;
                }
                return Ok(parse_nordic_power(payload));
            }
            Ok(_) => continue,
            Err(e) if e == "timeout" => return Ok(None),
            Err(e) => return Err(e),
        }
    }

    Ok(None)
}

fn telink_get(handle: HANDLE, buf: &mut [u8]) -> bool {
    buf.fill(0);
    buf[0] = DS_TELINK_REPORT_ID;
    unsafe { HidD_GetFeature(handle, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) }
}

fn telink_send(handle: HANDLE, feature_len: usize, opcode: u8) -> bool {
    let mut buf = vec![0u8; feature_len];
    let cmd = build_telink_feature(opcode);
    buf[..DS_TELINK_REPORT_LEN].copy_from_slice(&cmd);
    unsafe { HidD_SetFeature(handle, buf.as_ptr() as *const c_void, buf.len() as u32) }
}

fn telink_await(handle: HANDLE, opcode: u8, timeout_ms: u64, buf: &mut [u8]) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if telink_get(handle, buf) && is_telink_echo(&buf[..DS_TELINK_REPORT_LEN], opcode) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(TELINK_POLL_STEP_MS));
    }
}

fn query_telink(handle: HANDLE, caps: &HIDP_CAPS, wireless: bool) -> Result<Option<u8>, String> {
    let feature_len = (caps.FeatureReportByteLength as usize).max(DS_TELINK_REPORT_LEN);
    let mut resp = vec![0u8; feature_len];

    if wireless {
        if telink_get(handle, &mut resp)
            && is_telink_echo(&resp[..DS_TELINK_REPORT_LEN], DS_TELINK_OP_IDENTIFY)
        {
            if !telink_send(handle, feature_len, DS_TELINK_OP_BOND) {
                return Err(format!("Telink primer failed ({})", unsafe {
                    GetLastError()
                }));
            }
            if !telink_await(handle, DS_TELINK_OP_BOND, PRIMER_TIMEOUT_MS, &mut resp) {
                return Ok(None);
            }
        }
        if !telink_send(handle, feature_len, DS_TELINK_OP_IDENTIFY) {
            return Err(format!("HidD_SetFeature failed ({})", unsafe {
                GetLastError()
            }));
        }
        if telink_await(handle, DS_TELINK_OP_IDENTIFY, TELINK_TIMEOUT_MS, &mut resp) {
            return Ok(parse_telink_power(&resp[..DS_TELINK_REPORT_LEN]));
        }
        return Ok(None);
    }

    for _ in 0..3 {
        if !telink_send(handle, feature_len, DS_TELINK_OP_IDENTIFY) {
            return Err(format!("HidD_SetFeature failed ({})", unsafe {
                GetLastError()
            }));
        }
        sleep(Duration::from_millis(80));
        if telink_get(handle, &mut resp)
            && is_telink_echo(&resp[..DS_TELINK_REPORT_LEN], DS_TELINK_OP_IDENTIFY)
        {
            return Ok(parse_telink_power(&resp[..DS_TELINK_REPORT_LEN]));
        }
        sleep(Duration::from_millis(100));
    }

    Ok(None)
}
