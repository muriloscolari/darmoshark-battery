// SPDX-License-Identifier: GPL-2.0-or-later
//! Darmoshark (MOJI) vendor HID protocol — Rust port of `darmoshark_protocol.h`.

pub const DS_VENDOR_NORDIC: u16 = 0x1915;
pub const DS_PID_M3S_PRO_WIRED: u16 = 0x073c;
pub const DS_PID_M3S_PRO_RECEIVER: u16 = 0x0723;

pub const DS_VENDOR_TELINK: u16 = 0x248a;
pub const DS_PID_N3_WIRED: u16 = 0xff10;
pub const DS_PID_M3_WIRED: u16 = 0xff12;
pub const DS_PID_M3S_WIRED: u16 = 0xff18;
pub const DS_PID_TELINK_RECEIVER_1K: u16 = 0xff30;
pub const DS_PID_TELINK_RECEIVER_2K: u16 = 0xff31;

#[cfg_attr(not(windows), allow(dead_code))]
pub const NORDIC_USAGE_PAGE: u16 = 0xff0a;
#[cfg_attr(not(windows), allow(dead_code))]
pub const NORDIC_USAGE: u16 = 0x0001;

#[cfg_attr(not(windows), allow(dead_code))]
pub const TELINK_USAGE_PAGE: u16 = 0x008c;
#[cfg_attr(not(windows), allow(dead_code))]
pub const TELINK_USAGE: u16 = 0x0001;

pub const DS_REPORT_LEN: usize = 64;
pub const DS_CHECKSUM_TARGET: u8 = 0xa1;
pub const DS_WIRELESS_FLAG: u8 = 0x40;

pub const DS_TYPE_POWER: u8 = 0x01;
pub const DS_OP_BASE_INFO: u8 = 129;
pub const DS_POWER_SUB: u8 = 1;

pub const DS_TELINK_REPORT_ID: u8 = 0x51;
pub const DS_TELINK_REPORT_LEN: usize = 21;
pub const DS_TELINK_OP_BOND: u8 = 0x03;
pub const DS_TELINK_OP_IDENTIFY: u8 = 0x06;
pub const DS_TELINK_STATUS_OK: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Nordic,
    Telink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSpec {
    pub transport: Transport,
    pub wireless: bool,
    pub model: &'static str,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl DeviceSpec {
    pub fn usage_page(self) -> u16 {
        match self.transport {
            Transport::Nordic => NORDIC_USAGE_PAGE,
            Transport::Telink => TELINK_USAGE_PAGE,
        }
    }

    pub fn usage(self) -> u16 {
        match self.transport {
            Transport::Nordic => NORDIC_USAGE,
            Transport::Telink => TELINK_USAGE,
        }
    }
}

pub fn identify_device(vid: u16, pid: u16) -> Option<DeviceSpec> {
    match (vid, pid) {
        (DS_VENDOR_NORDIC, DS_PID_M3S_PRO_WIRED) => Some(DeviceSpec {
            transport: Transport::Nordic,
            wireless: false,
            model: "Darmoshark M3S Pro",
        }),
        (DS_VENDOR_NORDIC, DS_PID_M3S_PRO_RECEIVER) => Some(DeviceSpec {
            transport: Transport::Nordic,
            wireless: true,
            model: "Darmoshark M3S Pro (2.4 GHz)",
        }),
        (DS_VENDOR_TELINK, DS_PID_N3_WIRED) => Some(DeviceSpec {
            transport: Transport::Telink,
            wireless: false,
            model: "Darmoshark N3",
        }),
        (DS_VENDOR_TELINK, DS_PID_M3_WIRED) => Some(DeviceSpec {
            transport: Transport::Telink,
            wireless: false,
            model: "Darmoshark M3",
        }),
        (DS_VENDOR_TELINK, DS_PID_M3S_WIRED) => Some(DeviceSpec {
            transport: Transport::Telink,
            wireless: false,
            model: "Darmoshark M3S",
        }),
        (DS_VENDOR_TELINK, DS_PID_TELINK_RECEIVER_1K) => Some(DeviceSpec {
            transport: Transport::Telink,
            wireless: true,
            model: "Darmoshark M3 (2.4 GHz)",
        }),
        (DS_VENDOR_TELINK, DS_PID_TELINK_RECEIVER_2K) => Some(DeviceSpec {
            transport: Transport::Telink,
            wireless: true,
            model: "Darmoshark M3 (2.4 GHz 2K)",
        }),
        _ => None,
    }
}

/// Build the 64-byte Nordic battery/handshake query `[1, 0, 129, 1]`
/// (with the 2.4 GHz bit when `wireless` is true) and checksum byte.
pub fn build_nordic_power(wireless: bool) -> [u8; DS_REPORT_LEN] {
    let mut out = [0u8; DS_REPORT_LEN];
    out[0] = DS_TYPE_POWER | if wireless { DS_WIRELESS_FLAG } else { 0 };
    out[2] = DS_OP_BASE_INFO;
    out[3] = DS_POWER_SUB;

    let sum: u32 = out[..DS_REPORT_LEN - 1].iter().map(|&b| b as u32).sum();
    out[DS_REPORT_LEN - 1] = DS_CHECKSUM_TARGET.wrapping_sub((sum & 0xff) as u8);
    out
}

/// Whether a 64-byte Nordic input report is the answer to the power query.
pub fn is_nordic_power_response(resp: &[u8]) -> bool {
    resp.len() >= 4 && (resp[0] & 0x3f) == DS_TYPE_POWER && resp[3] == DS_POWER_SUB
}

/// Parse the battery percentage out of a Nordic power response (`dms` or `dms_v2`).
pub fn parse_nordic_power(resp: &[u8]) -> Option<u8> {
    if resp.len() < 12 {
        return None;
    }

    let dms_v2 = resp[6] == 0x15 && resp[7] == 0x19;
    let value = if dms_v2 { resp[11] } else { resp[6] };
    if value > 100 {
        return None;
    }

    Some(value)
}

/// Build a 21-byte Telink feature report `[0x51, opcode, 0, ...]`.
pub fn build_telink_feature(opcode: u8) -> [u8; DS_TELINK_REPORT_LEN] {
    let mut out = [0u8; DS_TELINK_REPORT_LEN];
    out[0] = DS_TELINK_REPORT_ID;
    out[1] = opcode;
    out
}

/// Whether a Telink feature report buffer echoes `opcode`.
pub fn is_telink_echo(resp: &[u8], opcode: u8) -> bool {
    resp.len() >= 2 && resp[0] == DS_TELINK_REPORT_ID && resp[1] == opcode
}

/// Parse the battery percentage out of a Telink `0x51` identify (`0x06`) response.
pub fn parse_telink_power(resp: &[u8]) -> Option<u8> {
    if resp.len() < 12 {
        return None;
    }
    if !is_telink_echo(resp, DS_TELINK_OP_IDENTIFY) {
        return None;
    }
    if resp[2] != DS_TELINK_STATUS_OK {
        return None;
    }

    let value = resp[11];
    if !(1..=100).contains(&value) {
        return None;
    }

    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nordic_usb_handshake_vector() {
        let out = build_nordic_power(false);
        assert_eq!(out[0], 1);
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 129);
        assert_eq!(out[3], 1);
        assert_eq!(out[DS_REPORT_LEN - 1], 30);
        let sum: u32 = out.iter().map(|&b| b as u32).sum();
        assert_eq!((sum & 0xff) as u8, DS_CHECKSUM_TARGET);
    }

    #[test]
    fn nordic_wireless_handshake_vector() {
        let out = build_nordic_power(true);
        assert_eq!(out[0], 1 | DS_WIRELESS_FLAG);
        let sum: u32 = out.iter().map(|&b| b as u32).sum();
        assert_eq!((sum & 0xff) as u8, DS_CHECKSUM_TARGET);
    }

    #[test]
    fn nordic_response_matching() {
        let mut resp = [0u8; DS_REPORT_LEN];
        resp[0] = 1;
        resp[3] = 1;
        assert!(is_nordic_power_response(&resp));
        resp[0] = 0x41;
        assert!(is_nordic_power_response(&resp));
        resp[0] = 4;
        assert!(!is_nordic_power_response(&resp));
        resp[0] = 1;
        assert!(!is_nordic_power_response(&resp[..3]));
    }

    #[test]
    fn nordic_dms_and_dms_v2_parsing() {
        let mut resp = [0u8; DS_REPORT_LEN];
        resp[5] = 1;
        resp[6] = 42;
        resp[7] = 2;
        resp[10] = 3;
        assert_eq!(parse_nordic_power(&resp), Some(42));

        let mut resp_v2 = [0u8; DS_REPORT_LEN];
        resp_v2[6] = 0x15;
        resp_v2[7] = 0x19;
        resp_v2[10] = 3;
        resp_v2[11] = 77;
        assert_eq!(parse_nordic_power(&resp_v2), Some(77));

        let mut bad = [0u8; DS_REPORT_LEN];
        bad[6] = 200;
        assert_eq!(parse_nordic_power(&bad), None);
        assert_eq!(parse_nordic_power(&resp[..11]), None);
    }

    #[test]
    fn telink_feature_builders_and_echo() {
        let bond = build_telink_feature(DS_TELINK_OP_BOND);
        assert_eq!(bond[0], DS_TELINK_REPORT_ID);
        assert_eq!(bond[1], DS_TELINK_OP_BOND);
        assert!(bond[2..].iter().all(|&b| b == 0));

        let id = build_telink_feature(DS_TELINK_OP_IDENTIFY);
        assert_eq!(id[0], DS_TELINK_REPORT_ID);
        assert_eq!(id[1], DS_TELINK_OP_IDENTIFY);
        assert!(id[2..].iter().all(|&b| b == 0));

        assert!(is_telink_echo(&id, DS_TELINK_OP_IDENTIFY));
        assert!(!is_telink_echo(&id, DS_TELINK_OP_BOND));
        assert!(!is_telink_echo(&id[..1], DS_TELINK_OP_IDENTIFY));

        let mut wrong_id = id;
        wrong_id[0] = 0x52;
        assert!(!is_telink_echo(&wrong_id, DS_TELINK_OP_IDENTIFY));
    }

    #[test]
    fn telink_power_parsing() {
        let mut telink = [0u8; DS_TELINK_REPORT_LEN];
        telink[0] = DS_TELINK_REPORT_ID;
        telink[1] = DS_TELINK_OP_IDENTIFY;
        telink[2] = DS_TELINK_STATUS_OK;
        telink[11] = 87;
        assert_eq!(parse_telink_power(&telink), Some(87));

        telink[11] = 0;
        assert_eq!(parse_telink_power(&telink), None);

        telink[11] = 101;
        assert_eq!(parse_telink_power(&telink), None);

        telink[11] = 87;
        telink[2] = 0;
        assert_eq!(parse_telink_power(&telink), None);

        telink[2] = DS_TELINK_STATUS_OK;
        telink[1] = DS_TELINK_OP_BOND;
        assert_eq!(parse_telink_power(&telink), None);

        telink[1] = DS_TELINK_OP_IDENTIFY;
        assert_eq!(parse_telink_power(&telink[..11]), None);
    }

    #[test]
    fn supported_device_lookup() {
        assert!(identify_device(0x1915, 0x0723).unwrap().wireless);
        assert!(!identify_device(0x1915, 0x073c).unwrap().wireless);
        assert!(identify_device(0x248a, 0xff30).unwrap().wireless);
        assert!(identify_device(0x248a, 0xff31).unwrap().wireless);
        assert!(!identify_device(0x248a, 0xff12).unwrap().wireless);
        assert_eq!(identify_device(0x1234, 0x5678), None);
    }
}
