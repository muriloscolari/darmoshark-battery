// SPDX-License-Identifier: GPL-2.0-or-later
//! Darmoshark mouse battery monitor — minimalist Windows taskbar widget.

#![cfg_attr(all(not(test), target_os = "windows"), windows_subsystem = "windows")]

mod protocol;

#[cfg(windows)]
mod hid;
#[cfg(windows)]
mod tray_icon;

#[cfg(not(windows))]
fn main() {
    eprintln!("darmoshark-battery-tray is a Windows application. Run `cargo test` for protocol tests.");
}

#[cfg(windows)]
fn main() {
    win_app::run();
}

#[cfg(windows)]
mod win_app {
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use windows_sys::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT,
        WPARAM,
    };
    use windows_sys::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, FindWindowExW, FindWindowW, GetCursorPos, GetMessageW,
        GetWindowLongPtrW, GetWindowRect, IsWindowVisible, PostMessageW, PostQuitMessage,
        RegisterClassExW, RegisterDeviceNotificationW, SetForegroundWindow, SetTimer,
        SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage,
        UnregisterDeviceNotification, CREATESTRUCTW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE,
        DBT_DEVNODES_CHANGED, DEVICE_NOTIFY_WINDOW_HANDLE, GWLP_HWNDPARENT, GWLP_USERDATA,
        HWND_TOPMOST, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, TPM_BOTTOMALIGN,
        TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CREATE, WM_DESTROY,
        WM_DEVICECHANGE, WM_LBUTTONUP, WM_NCCREATE, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
        WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    use crate::hid;
    use crate::tray_icon::update_widget_surface;

    const WM_APP_BATTERY_UPDATE: u32 = WM_APP + 1;
    const TIMER_LAYOUT_ID: usize = 1;
    const LAYOUT_CHECK_MS: u32 = 1000;

    const IDM_STATUS: usize = 1000;
    const IDM_REFRESH: usize = 1001;
    const IDM_AUTOSTART: usize = 1002;
    const IDM_EXIT: usize = 1003;

    const POLL_INTERVAL: Duration = Duration::from_secs(60);
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const RUN_VALUE: &str = "DarmosharkBattery";
    const PERSONALIZE_KEY: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
    const LIGHT_THEME_VALUE: &str = "SystemUsesLightTheme";

    #[repr(C)]
    struct DevBroadcastDeviceInterfaceW {
        dbcc_size: u32,
        dbcc_devicetype: u32,
        dbcc_reserved: u32,
        dbcc_classguid: windows_sys::core::GUID,
        dbcc_name: [u16; 1],
    }

    const DBT_DEVTYP_DEVICEINTERFACE: u32 = 0x00000005;

    #[derive(Clone, Default)]
    struct BatteryState {
        model: Option<&'static str>,
        percent: Option<u8>,
        sleeping: bool,
        error: Option<String>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct RenderKey {
        percent: Option<u8>,
        light_taskbar: bool,
        anchor_x: i32,
        anchor_y: i32,
        height: i32,
        dpi: u32,
    }

    struct AppState {
        hwnd: HWND,
        owner_tray: HWND,
        last_render: Option<RenderKey>,
        visible: bool,
        dev_notify: *mut std::ffi::c_void,
        battery: Mutex<BatteryState>,
        wake: Arc<(Mutex<bool>, Condvar)>,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn run() {
        let mutex_name = wide("Local\\DarmosharkBatteryTray");
        let mutex = unsafe { CreateMutexW(null(), 1, mutex_name.as_ptr()) };
        if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(mutex) };
            return;
        }

        let hinstance = unsafe { GetModuleHandleW(null()) };
        let class_name = wide("DarmosharkBatteryWidget");

        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name.as_ptr(),
            ..unsafe { zeroed() }
        };

        if unsafe { RegisterClassExW(&wc) } == 0 {
            if !mutex.is_null() {
                unsafe { CloseHandle(mutex) };
            }
            return;
        }

        let wake = Arc::new((Mutex::new(false), Condvar::new()));

        let mut app = Box::new(AppState {
            hwnd: null_mut(),
            owner_tray: null_mut(),
            last_render: None,
            visible: false,
            dev_notify: null_mut(),
            battery: Mutex::new(BatteryState::default()),
            wake: Arc::clone(&wake),
        });

        let title = wide("Darmoshark Battery");
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                hinstance,
                (&mut *app as *mut AppState).cast(),
            )
        };

        if hwnd.is_null() {
            if !mutex.is_null() {
                unsafe { CloseHandle(mutex) };
            }
            return;
        }

        let hwnd_val = hwnd as usize;
        let app_ptr = (&*app as *const AppState) as usize;
        let worker_wake = Arc::clone(&wake);
        let stop = Arc::new(Mutex::new(false));
        let worker_stop = Arc::clone(&stop);

        let worker = std::thread::spawn(move || {
            let app = unsafe { &*(app_ptr as *const AppState) };
            loop {
                let res = hid::poll_battery();
                {
                    let mut st = app.battery.lock().unwrap();
                    match res {
                        Ok(Some(reading)) => {
                            let same_model = st.model == Some(reading.model);
                            st.model = Some(reading.model);
                            st.error = None;
                            if let Some(p) = reading.percent {
                                st.percent = Some(p);
                                st.sleeping = false;
                            } else {
                                if !same_model {
                                    st.percent = None;
                                }
                                st.sleeping = true;
                            }
                        }
                        Ok(None) => {
                            st.model = None;
                            st.percent = None;
                            st.sleeping = false;
                            st.error = None;
                        }
                        Err(e) => {
                            st.error = Some(e);
                        }
                    }
                }

                unsafe {
                    PostMessageW(hwnd_val as HWND, WM_APP_BATTERY_UPDATE, 0, 0);
                }

                let (lock, cvar) = &*worker_wake;
                let mut triggered = lock.lock().unwrap();
                if !*triggered {
                    let (guard, _) = cvar.wait_timeout(triggered, POLL_INTERVAL).unwrap();
                    triggered = guard;
                }
                *triggered = false;
                if *worker_stop.lock().unwrap() {
                    break;
                }
            }
        });

        let mut msg: MSG = unsafe { zeroed() };
        while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        *stop.lock().unwrap() = true;
        trigger_poll(&wake);
        let _ = worker.join();

        if !mutex.is_null() {
            unsafe { CloseHandle(mutex) };
        }
    }

    fn trigger_poll(wake: &(Mutex<bool>, Condvar)) {
        let (lock, cvar) = wake;
        let mut triggered = lock.lock().unwrap();
        *triggered = true;
        cvar.notify_one();
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_NCCREATE {
            let cs = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let app = &mut *ptr;

        match msg {
            WM_CREATE => {
                app.hwnd = hwnd;
                let mut hid_guid = zeroed();
                HidD_GetHidGuid(&mut hid_guid);
                let filter = DevBroadcastDeviceInterfaceW {
                    dbcc_size: size_of::<DevBroadcastDeviceInterfaceW>() as u32,
                    dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE,
                    dbcc_reserved: 0,
                    dbcc_classguid: hid_guid,
                    dbcc_name: [0],
                };
                app.dev_notify = RegisterDeviceNotificationW(
                    hwnd,
                    (&filter as *const DevBroadcastDeviceInterfaceW).cast(),
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                );
                SetTimer(hwnd, TIMER_LAYOUT_ID, LAYOUT_CHECK_MS, None);
                sync_widget(app, true);
                0
            }
            WM_APP_BATTERY_UPDATE => {
                sync_widget(app, false);
                0
            }
            WM_SETTINGCHANGE => {
                sync_widget(app, true);
                0
            }
            WM_TIMER => {
                if wparam == TIMER_LAYOUT_ID {
                    sync_widget(app, false);
                }
                0
            }
            WM_DEVICECHANGE => {
                let event = wparam as u32;
                if event == DBT_DEVICEARRIVAL
                    || event == DBT_DEVICEREMOVECOMPLETE
                    || event == DBT_DEVNODES_CHANGED
                {
                    trigger_poll(&app.wake);
                }
                0
            }
            WM_RBUTTONUP => {
                show_context_menu(app);
                0
            }
            WM_LBUTTONUP => {
                trigger_poll(&app.wake);
                0
            }
            WM_COMMAND => {
                let cmd = wparam & 0xffff;
                match cmd {
                    IDM_REFRESH => trigger_poll(&app.wake),
                    IDM_AUTOSTART => toggle_autostart(),
                    IDM_EXIT => PostQuitMessage(0),
                    _ => {}
                }
                0
            }
            WM_DESTROY => {
                if !app.dev_notify.is_null() {
                    UnregisterDeviceNotification(app.dev_notify);
                    app.dev_notify = null_mut();
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    fn format_status(state: &BatteryState) -> String {
        match (state.model, state.percent, state.sleeping, &state.error) {
            (Some(model), Some(p), true, _) => format!("{model}: {p}% (asleep)"),
            (Some(model), Some(p), false, _) => format!("{model}: {p}%"),
            (Some(model), None, true, _) => format!("{model}: -- (asleep)"),
            (Some(model), None, false, _) => format!("{model}: --"),
            (None, _, _, Some(err)) => format!("Darmoshark: {err}"),
            (None, _, _, None) => "Darmoshark: Disconnected".to_string(),
        }
    }

    fn get_system_dpi() -> u32 {
        unsafe {
            let hdc = GetDC(null_mut());
            if hdc.is_null() {
                return 96;
            }
            let dpi = GetDeviceCaps(hdc, LOGPIXELSX as i32) as u32;
            ReleaseDC(null_mut(), hdc);
            dpi.max(96)
        }
    }

    fn find_taskbar_anchor() -> Option<(HWND, POINT, i32)> {
        let tray_cls = wide("Shell_TrayWnd");
        let notify_cls = wide("TrayNotifyWnd");
        unsafe {
            let shell_tray = FindWindowW(tray_cls.as_ptr(), null());
            if shell_tray.is_null() || IsWindowVisible(shell_tray) == 0 {
                return None;
            }
            let notify = FindWindowExW(shell_tray, null_mut(), notify_cls.as_ptr(), null());
            if notify.is_null() {
                return None;
            }
            let mut rc: RECT = zeroed();
            if GetWindowRect(notify, &mut rc) == 0 {
                return None;
            }
            let h = rc.bottom - rc.top;
            if h <= 0 {
                return None;
            }
            // Place right to the left of the tray chevron (^).
            Some((shell_tray, POINT { x: rc.left - 2, y: rc.top }, h))
        }
    }

    fn is_light_taskbar() -> bool {
        let key_w = wide(PERSONALIZE_KEY);
        let val_w = wide(LIGHT_THEME_VALUE);
        let mut hkey = null_mut();
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_READ, &mut hkey)
        };
        if rc != 0 {
            return false;
        }
        let mut data: u32 = 0;
        let mut size: u32 = size_of::<u32>() as u32;
        let ok = unsafe {
            RegQueryValueExW(
                hkey,
                val_w.as_ptr(),
                null_mut(),
                null_mut(),
                (&mut data as *mut u32).cast(),
                &mut size,
            ) == 0
        };
        unsafe { RegCloseKey(hkey) };
        ok && data != 0
    }

    unsafe fn sync_widget(app: &mut AppState, force: bool) {
        let Some((shell_tray, anchor, height)) = find_taskbar_anchor() else {
            if app.visible {
                ShowWindow(app.hwnd, SW_HIDE);
                app.visible = false;
            }
            return;
        };

        let mut establish_z = false;
        if shell_tray != app.owner_tray {
            SetWindowLongPtrW(app.hwnd, GWLP_HWNDPARENT, shell_tray as isize);
            app.owner_tray = shell_tray;
            establish_z = true;
        }

        let percent = app.battery.lock().unwrap().percent;
        let light_taskbar = is_light_taskbar();
        let dpi = get_system_dpi();
        let key = RenderKey {
            percent,
            light_taskbar,
            anchor_x: anchor.x,
            anchor_y: anchor.y,
            height,
            dpi,
        };

        if force || app.last_render != Some(key) {
            if update_widget_surface(app.hwnd, anchor, height, dpi, percent, light_taskbar).is_some()
            {
                app.last_render = Some(key);
            }
        }

        if !app.visible {
            ShowWindow(app.hwnd, SW_SHOWNA);
            app.visible = true;
            establish_z = true;
        }

        if establish_z {
            // Establish TOPMOST above the newly attached Shell_TrayWnd owner without stealing focus.
            SetWindowPos(
                app.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    }

    unsafe fn show_context_menu(app: &AppState) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }

        let status = {
            let st = app.battery.lock().unwrap();
            format_status(&st)
        };
        let status_w = wide(&status);
        let refresh_w = wide("Refresh now");
        let autostart_w = wide("Start with Windows");
        let exit_w = wide("Exit");

        AppendMenuW(menu, MF_STRING | MF_GRAYED, IDM_STATUS, status_w.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, IDM_REFRESH, refresh_w.as_ptr());

        let auto_flags = MF_STRING
            | if is_autostart_enabled() {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            };
        AppendMenuW(menu, auto_flags, IDM_AUTOSTART, autostart_w.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, IDM_EXIT, exit_w.as_ptr());

        let mut pt: POINT = zeroed();
        GetCursorPos(&mut pt);
        SetForegroundWindow(app.hwnd);
        TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            null(),
        );
        DestroyMenu(menu);
    }

    fn is_autostart_enabled() -> bool {
        let key_w = wide(RUN_KEY);
        let val_w = wide(RUN_VALUE);
        let mut hkey = null_mut();
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_READ, &mut hkey)
        };
        if rc != 0 {
            return false;
        }
        let exists = unsafe {
            RegQueryValueExW(
                hkey,
                val_w.as_ptr(),
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            ) == 0
        };
        unsafe { RegCloseKey(hkey) };
        exists
    }

    fn toggle_autostart() {
        let key_w = wide(RUN_KEY);
        let val_w = wide(RUN_VALUE);
        let mut hkey = null_mut();
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_WRITE, &mut hkey)
        };
        if rc != 0 {
            return;
        }

        if is_autostart_enabled() {
            unsafe { RegDeleteValueW(hkey, val_w.as_ptr()) };
        } else {
            let mut exe_buf = vec![0u16; 32768];
            let len = unsafe {
                GetModuleFileNameW(null_mut(), exe_buf.as_mut_ptr(), exe_buf.len() as u32)
            } as usize;
            if len > 0 && len < exe_buf.len() {
                let path = String::from_utf16_lossy(&exe_buf[..len]);
                let quoted = wide(&format!("\"{path}\""));
                let byte_len = (quoted.len() * 2) as u32;
                unsafe {
                    RegSetValueExW(
                        hkey,
                        val_w.as_ptr(),
                        0,
                        REG_SZ,
                        quoted.as_ptr().cast(),
                        byte_len,
                    );
                }
            }
        }

        unsafe { RegCloseKey(hkey) };
    }
}
