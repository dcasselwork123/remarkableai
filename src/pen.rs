//! Raw evdev pen input, opened SHARED — xochitl keeps receiving the pen and
//! draws normally; scribe only listens. (Riddle grabs the device; scribe must
//! never grab, or the tablet stops inking.)
//!
//! rM2 Wacom digitizer is rotated 90° relative to the screen: raw X runs
//! along the screen's LONG edge (bottom→top), raw Y along the short edge.

use crate::screen::{SCREEN_H, SCREEN_W};

pub const DIGI_MAX_X: i32 = 20967;
pub const DIGI_MAX_Y: i32 = 15725;

/// Raw digitizer coordinates → screen pixels (same mapping KOReader uses).
#[inline]
pub fn to_screen(raw_x: i32, raw_y: i32) -> (i32, i32) {
    (
        raw_y * (SCREEN_W as i32 - 1) / DIGI_MAX_Y,
        (DIGI_MAX_X - raw_x) * (SCREEN_H as i32 - 1) / DIGI_MAX_X,
    )
}

/// Screen pixels → raw digitizer coordinates (for injection).
#[inline]
pub fn to_raw(x: i32, y: i32) -> (i32, i32) {
    (
        DIGI_MAX_X - y * DIGI_MAX_X / (SCREEN_H as i32 - 1),
        x * DIGI_MAX_Y / (SCREEN_W as i32 - 1),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
}

#[derive(Debug, Clone, Copy)]
pub struct PenSample {
    /// Screen coordinates.
    pub x: i32,
    pub y: i32,
    /// 0..4096 (carried for future brush-width use; unread today)
    #[allow(dead_code)]
    pub pressure: i32,
    pub tool: Tool,
    pub touching: bool,
    /// True from tool-in-range until the pen leaves the digitizer.
    pub proximity: bool,
}

#[cfg(unix)]
pub use dev::PenDevice;

#[cfg(unix)]
mod dev {
    use super::{to_screen, PenSample, Tool};
    use crate::evdev::{
        ABS_PRESSURE, ABS_X, ABS_Y, BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, EV_ABS, EV_KEY,
        EV_SYN, SYN_REPORT,
    };
    use std::io;
    use std::os::fd::RawFd;

    /// The kernel dropped events on this client (our buffer overflowed —
    /// guaranteed to happen while scribe's own injections loop back). Any
    /// key transition (e.g. BTN_TOOL_PEN 0) may have been lost, so state
    /// must be re-queried, or proximity can wedge true forever.
    const SYN_DROPPED: u16 = 3;

    pub struct PenDevice {
        fd: RawFd,
        path: String,
        dbg_left: u32,
        // Accumulated state between SYN_REPORTs.
        raw_x: i32,
        raw_y: i32,
        pressure: i32,
        tool: Tool,
        touching: bool,
        pen_in_range: bool,
        rubber_in_range: bool,
        proximity: bool,
        dirty: bool,
    }

    impl PenDevice {
        /// Find and open the marker input device WITHOUT grabbing it.
        pub fn open_shared() -> io::Result<Self> {
            let path = find_marker_device()?;
            let cpath = std::ffi::CString::new(path.clone()).unwrap();
            let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            eprintln!("scribe: pen device {path} opened (shared)");
            Ok(Self {
                fd,
                path,
                dbg_left: if std::env::var_os("SCRIBE_DEBUG_PEN").is_some() { 60 } else { 0 },
                raw_x: 0,
                raw_y: 0,
                pressure: 0,
                tool: Tool::Pen,
                touching: false,
                pen_in_range: false,
                rubber_in_range: false,
                proximity: false,
                dirty: false,
            })
        }

        pub fn raw_fd(&self) -> RawFd {
            self.fd
        }

        /// Device node path — the injector writes to the same node.
        pub fn path(&self) -> &str {
            &self.path
        }

        pub fn proximity(&self) -> bool {
            self.proximity
        }

        /// Drain all pending events; returns one sample per SYN_REPORT frame
        /// that changed state.
        pub fn drain(&mut self) -> Vec<PenSample> {
            let mut out = Vec::new();
            let mut buf = [0u8; crate::evdev::EV_SIZE * 64];
            loop {
                let n = unsafe {
                    libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };
                if n <= 0 {
                    break;
                }
                for chunk in buf[..n as usize].chunks_exact(crate::evdev::EV_SIZE) {
                    let (etype, code, value) = crate::evdev::parse(chunk);
                    match (etype, code) {
                        (EV_ABS, ABS_X) => {
                            self.raw_x = value;
                            self.dirty = true;
                        }
                        (EV_ABS, ABS_Y) => {
                            self.raw_y = value;
                            self.dirty = true;
                        }
                        (EV_ABS, ABS_PRESSURE) => {
                            self.pressure = value;
                            self.dirty = true;
                        }
                        (EV_KEY, BTN_TOOL_PEN) => {
                            self.pen_in_range = value == 1;
                            if self.pen_in_range {
                                self.tool = Tool::Pen;
                            }
                            self.proximity = self.pen_in_range || self.rubber_in_range;
                            self.dirty = true;
                        }
                        (EV_KEY, BTN_TOOL_RUBBER) => {
                            self.rubber_in_range = value == 1;
                            if self.rubber_in_range {
                                self.tool = Tool::Eraser;
                            }
                            self.proximity = self.pen_in_range || self.rubber_in_range;
                            self.dirty = true;
                        }
                        (EV_KEY, BTN_TOUCH) => {
                            self.touching = value == 1;
                            self.dirty = true;
                        }
                        (EV_SYN, SYN_DROPPED) => {
                            self.resync_keys();
                        }
                        (EV_SYN, SYN_REPORT) => {
                            if self.dirty {
                                self.dirty = false;
                                let (x, y) = to_screen(self.raw_x, self.raw_y);
                                if self.dbg_left > 0 && self.touching {
                                    self.dbg_left -= 1;
                                    eprintln!(
                                        "scribe: pen raw=({},{}) p={} -> screen=({x},{y})",
                                        self.raw_x, self.raw_y, self.pressure
                                    );
                                }
                                out.push(PenSample {
                                    x,
                                    y,
                                    pressure: self.pressure,
                                    tool: self.tool,
                                    touching: self.touching,
                                    proximity: self.proximity,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            out
        }

        /// Drain and discard — used right after injection so scribe does not
        /// interpret its own synthetic strokes as user ink.
        pub fn discard(&mut self) {
            let _ = self.drain();
            self.touching = false;
        }

        /// Re-read the device's true key state (EVIOCGKEY) after SYN_DROPPED.
        fn resync_keys(&mut self) {
            // Bitmap sized for KEY_MAX (0x2ff) → 768 bits.
            let mut bits = [0u8; 96];
            // EVIOCGKEY(len) = _IOC(_IOC_READ, 'E', 0x18, len)
            let req: u32 = (2u32 << 30) | ((bits.len() as u32) << 16) | (0x45 << 8) | 0x18;
            let n = unsafe { libc::ioctl(self.fd, req as _, bits.as_mut_ptr()) };
            if n >= 0 {
                let bit = |code: u16| bits[code as usize / 8] & (1 << (code % 8)) != 0;
                self.pen_in_range = bit(BTN_TOOL_PEN);
                self.rubber_in_range = bit(BTN_TOOL_RUBBER);
                self.touching = bit(BTN_TOUCH);
            } else {
                // Conservative: assume released; the hardware re-asserts
                // tool-in-range next time the pen approaches.
                self.pen_in_range = false;
                self.rubber_in_range = false;
                self.touching = false;
            }
            self.proximity = self.pen_in_range || self.rubber_in_range;
            if self.rubber_in_range {
                self.tool = Tool::Eraser;
            } else if self.pen_in_range {
                self.tool = Tool::Pen;
            }
            self.dirty = true;
            eprintln!(
                "scribe: pen events dropped — resynced (prox={} touch={})",
                self.proximity, self.touching
            );
        }
    }

    impl Drop for PenDevice {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.fd);
            }
        }
    }

    fn find_marker_device() -> io::Result<String> {
        for i in 0..8 {
            let name_path = format!("/sys/class/input/event{i}/device/name");
            if let Ok(name) = std::fs::read_to_string(&name_path) {
                // reMarkable 1/2: "Wacom I2C Digitizer".
                if name.to_lowercase().contains("wacom") {
                    return Ok(format!("/dev/input/event{i}"));
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no wacom input device found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_screen_roundtrip_is_tight() {
        // Screen → raw → screen must land within a pixel or two everywhere,
        // or injected ink would visibly drift from where the reply was laid out.
        for &(x, y) in &[
            (0, 0),
            (1403, 0),
            (0, 1871),
            (1403, 1871),
            (702, 936),
            (100, 1700),
            (1300, 50),
        ] {
            let (rx, ry) = to_raw(x, y);
            assert!((0..=DIGI_MAX_X).contains(&rx), "raw x out of range for ({x},{y})");
            assert!((0..=DIGI_MAX_Y).contains(&ry), "raw y out of range for ({x},{y})");
            let (sx, sy) = to_screen(rx, ry);
            assert!((sx - x).abs() <= 2, "x drift {x}->{sx}");
            assert!((sy - y).abs() <= 2, "y drift {y}->{sy}");
        }
    }
}
