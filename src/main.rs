//! scribe — an AI margin assistant that lives inside the reMarkable 2's
//! normal notebook mode.
//!
//! Write in xochitl as usual. Circle some ink and HOLD the pen still for a
//! second: the circle vanishes (acknowledged), the circled handwriting goes
//! to a vision LLM, and the reply is written back onto the page in its place
//! — as real ink, injected through the digitizer, so it saves and syncs like
//! anything else you write. Write an instruction inside the circle ("translate
//! to French", "make this a list") and it does that instead. A four-finger
//! tap runs the same flow on everything scribe has seen on the page.

mod auth;
mod drawing;
mod evdev;
mod oracle;
mod pen;
mod screen;
mod script;
mod shadow;
mod trigger;

#[cfg(unix)]
mod inject;
#[cfg(unix)]
mod touch;

#[cfg(unix)]
mod daemon;

fn main() {
    #[cfg(unix)]
    {
        daemon::run();
    }
    #[cfg(not(unix))]
    {
        eprintln!(
            "scribe only runs on the reMarkable 2. Cross-build with:\n  docker run --rm -v \
             \"$PWD\":/home/rust/src messense/rust-musl-cross:armv7-musleabihf cargo build --release\n\
             (`cargo test` works on any host.)"
        );
        std::process::exit(1);
    }
}
