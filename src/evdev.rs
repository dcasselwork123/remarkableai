//! Raw `struct input_event` framing: decoding for the pen/touch readers and
//! encoding for the injector.
//!
//! The kernel struct starts with a `struct timeval` whose size follows the
//! userland ABI: 8 bytes on 32-bit ARM (reMarkable 2) → 16-byte events; 16
//! bytes on 64-bit hosts → 24-byte events. Tests run on 64-bit hosts, the
//! device build is 32-bit; both use the same code via EV_SIZE.

#[cfg(target_pointer_width = "64")]
pub const EV_SIZE: usize = 24;
#[cfg(not(target_pointer_width = "64"))]
pub const EV_SIZE: usize = 16;

const TYPE_OFF: usize = EV_SIZE - 8;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_ABS: u16 = 3;
pub const SYN_REPORT: u16 = 0;
pub const ABS_X: u16 = 0;
pub const ABS_Y: u16 = 1;
pub const ABS_PRESSURE: u16 = 24;
pub const ABS_DISTANCE: u16 = 25;
pub const ABS_MT_SLOT: u16 = 47;
pub const ABS_MT_POSITION_X: u16 = 53;
pub const ABS_MT_POSITION_Y: u16 = 54;
pub const ABS_MT_TRACKING_ID: u16 = 57;
pub const BTN_TOOL_PEN: u16 = 320;
pub const BTN_TOOL_RUBBER: u16 = 321;
pub const BTN_TOUCH: u16 = 330;

/// Decode one event frame: (type, code, value).
#[inline]
pub fn parse(chunk: &[u8]) -> (u16, u16, i32) {
    (
        u16::from_le_bytes(chunk[TYPE_OFF..TYPE_OFF + 2].try_into().unwrap()),
        u16::from_le_bytes(chunk[TYPE_OFF + 2..TYPE_OFF + 4].try_into().unwrap()),
        i32::from_le_bytes(chunk[TYPE_OFF + 4..TYPE_OFF + 8].try_into().unwrap()),
    )
}

/// Encode one event frame with a zeroed timestamp — the kernel restamps
/// events injected by writing to an evdev node.
#[inline]
pub fn encode(etype: u16, code: u16, value: i32, out: &mut Vec<u8>) {
    let start = out.len();
    out.resize(start + EV_SIZE, 0);
    out[start + TYPE_OFF..start + TYPE_OFF + 2].copy_from_slice(&etype.to_le_bytes());
    out[start + TYPE_OFF + 2..start + TYPE_OFF + 4].copy_from_slice(&code.to_le_bytes());
    out[start + TYPE_OFF + 4..start + TYPE_OFF + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_parse_roundtrip() {
        let mut buf = Vec::new();
        encode(EV_ABS, ABS_PRESSURE, 3021, &mut buf);
        encode(EV_SYN, SYN_REPORT, 0, &mut buf);
        assert_eq!(buf.len(), 2 * EV_SIZE);
        assert_eq!(parse(&buf[..EV_SIZE]), (EV_ABS, ABS_PRESSURE, 3021));
        assert_eq!(parse(&buf[EV_SIZE..]), (EV_SYN, SYN_REPORT, 0));
    }

    #[test]
    fn encode_negative_value() {
        let mut buf = Vec::new();
        encode(EV_ABS, ABS_MT_TRACKING_ID, -1, &mut buf);
        assert_eq!(parse(&buf), (EV_ABS, ABS_MT_TRACKING_ID, -1));
    }
}
