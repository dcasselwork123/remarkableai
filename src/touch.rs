//! Raw multitouch, opened SHARED — xochitl keeps full control of the touch
//! screen; scribe only watches for the gestures it cares about:
//!
//!  - 2-finger tap  → xochitl undoes; scribe pops its shadow stroke to match
//!  - 4-finger tap  → whole-page AI action (unused by stock xochitl)
//!  - 1-finger horizontal swipe → xochitl turned the page; shadow resets
//!
//! rM2 cyttsp5 controller: X 0..1403 screen-aligned, Y 0..1871 with the
//! origin at the BOTTOM (inverted relative to the screen; normalized here).

use std::io;
use std::os::fd::RawFd;

use crate::evdev::{
    ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, EV_ABS, EV_SYN,
    SYN_REPORT,
};

const MAX_SLOTS: usize = 16;
const TOUCH_MAX_Y: i32 = 1871;
/// A gesture is a tap when no single finger traveled more than this
/// (|dx|+|dy| from its own touchdown point). Per-finger, not summed —
/// five fingers each jitter a little, and summing their jitter would
/// make a 5-finger tap nearly impossible to land.
const TAP_SLOP: i32 = 60;
/// A 1-finger travel of at least this, mostly horizontal, is a page turn.
const SWIPE_MIN: i32 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// All fingers lifted after a tap with this many fingers (2, 3, 4, …).
    Tap(u8),
    /// One-finger horizontal swipe — page turn in xochitl.
    PageSwipe,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    active: bool,
    start_x: i32,
    start_y: i32,
    x: i32,
    y: i32,
    started: bool,
}

pub struct TouchDevice {
    fd: RawFd,
    slots: [Slot; MAX_SLOTS],
    cur: usize,
    max_fingers: usize,
}

impl TouchDevice {
    /// Open WITHOUT grabbing: xochitl keeps receiving touch normally.
    pub fn open_shared() -> io::Result<Self> {
        for i in 0..8 {
            let name_path = format!("/sys/class/input/event{i}/device/name");
            if let Ok(name) = std::fs::read_to_string(&name_path) {
                let name = name.to_lowercase();
                // rM2 controller is "cyttsp5_mt" (newer units: "pt_mt").
                if name.contains("cyttsp5") || name.contains("pt_mt") || name.contains("touch") {
                    let path = std::ffi::CString::new(format!("/dev/input/event{i}")).unwrap();
                    let fd =
                        unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
                    if fd < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    eprintln!("scribe: touch device /dev/input/event{i} opened (shared)");
                    return Ok(Self {
                        fd,
                        slots: [Slot::default(); MAX_SLOTS],
                        cur: 0,
                        max_fingers: 0,
                    });
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no touch device"))
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Drain and discard touch input, then cancel every partial gesture.
    /// Used for palm rejection while the marker is in digitizer proximity.
    pub fn suppress(&mut self) {
        let _ = self.drain();
        self.slots = [Slot::default(); MAX_SLOTS];
        self.max_fingers = 0;
    }

    pub fn drain(&mut self) -> Vec<Gesture> {
        let mut out = Vec::new();
        let mut buf = [0u8; crate::evdev::EV_SIZE * 64];
        loop {
            let n =
                unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            for chunk in buf[..n as usize].chunks_exact(crate::evdev::EV_SIZE) {
                let (etype, code, value) = crate::evdev::parse(chunk);
                if etype == EV_ABS && code == ABS_MT_SLOT {
                    self.cur = (value.max(0) as usize).min(MAX_SLOTS - 1);
                } else if etype == EV_ABS && code == ABS_MT_POSITION_X {
                    let s = &mut self.slots[self.cur];
                    s.x = value;
                    if s.active && !s.started {
                        s.start_x = value;
                    }
                } else if etype == EV_ABS && code == ABS_MT_POSITION_Y {
                    // Normalize to screen orientation.
                    let value = TOUCH_MAX_Y - value;
                    let s = &mut self.slots[self.cur];
                    s.y = value;
                    if s.active && !s.started {
                        s.start_y = value;
                        s.started = true;
                    }
                } else if etype == EV_ABS && code == ABS_MT_TRACKING_ID {
                    if value != -1 {
                        self.slots[self.cur] = Slot {
                            active: true,
                            start_x: self.slots[self.cur].x,
                            start_y: self.slots[self.cur].y,
                            x: self.slots[self.cur].x,
                            y: self.slots[self.cur].y,
                            started: false,
                        };
                    } else {
                        self.slots[self.cur].active = false;
                    }
                } else if etype == EV_SYN && code == SYN_REPORT {
                    self.finish_frame(&mut out);
                }
            }
        }
        out
    }

    fn finish_frame(&mut self, out: &mut Vec<Gesture>) {
        let count = self.slots.iter().filter(|s| s.active).count();
        self.max_fingers = self.max_fingers.max(count);

        if count == 0 && self.max_fingers > 0 {
            // Released slots retain their coordinates.
            let travel =
                |s: &Slot| (s.x - s.start_x).abs() + (s.y - s.start_y).abs();
            let max_travel =
                self.slots.iter().filter(|s| s.started).map(travel).max().unwrap_or(0);
            if max_travel < TAP_SLOP {
                if self.max_fingers >= 2 {
                    out.push(Gesture::Tap(self.max_fingers.min(255) as u8));
                }
            } else if self.max_fingers == 1 {
                if let Some(slot) =
                    self.slots.iter().filter(|s| s.started).max_by_key(|s| travel(s))
                {
                    let dx = slot.x - slot.start_x;
                    let dy = slot.y - slot.start_y;
                    if dx.abs() >= SWIPE_MIN && dx.abs() >= 2 * dy.abs() {
                        out.push(Gesture::PageSwipe);
                    }
                }
            }
            self.slots = [Slot::default(); MAX_SLOTS];
            self.max_fingers = 0;
        }
    }
}

impl Drop for TouchDevice {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}
