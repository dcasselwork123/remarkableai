# CLAUDE.md

## What this is

**scribe** — a background daemon for the **reMarkable 2** that adds AI to the
*stock* xochitl notebook. Circle handwriting + hold the pen ~1 s → the
circled region goes to a vision LLM → the old ink is erased and the reply is
written back as real ink. Four-finger tap = whole-page version. Sibling of
the riddle fork at `C:\dev\remarkableapp\Riddle` (which is a takeover/qtfb
*app*; scribe rides alongside xochitl instead) and reuses its pen mapping,
OAuth, and handwriting-synthesis code.

## Architecture

```
pen evdev (SHARED — never EVIOCGRAB) ──► daemon.rs loop ──► shadow.rs (stroke mirror)
touch evdev (shared)  ─ gestures ────────┘   │ pen-up: trigger.rs (circle+hold?)
                                             ▼
                    shadow.rasterize_png(region) ──► oracle.rs (blocking, worker thread)
                                             ▼ reply text
                    inject.rs: erase_path per circled stroke (BTN_TOOL_RUBBER),
                    then script.rs layout → pen strokes (BTN_TOOL_PEN) written
                    into /dev/input/eventN — xochitl inks them as real strokes
```

- `src/pen.rs` — rM2 Wacom mapping (rotated 90°) + **`to_raw` inverse** used
  by the injector. Device opened shared; grabbing would kill xochitl inking.
- `src/inject.rs` — writes `input_event`s to the Wacom node. The kernel
  restamps times; BTN_TOOL_RUBBER erases regardless of the selected toolbar
  tool (same trick as the Lamy-button hacks). Erase = 3 offset retrace passes.
- `src/shadow.rs` — stroke store with stable ids; ops erase *by id* so user
  edits during the oracle wait can't shift targets. `epoch` bumps on page
  swipe; stale replies are dropped.
- `src/trigger.rs` — timing-first: motionless ≥900 ms tail (SCRIBE_HOLD_MS),
  then crude loop checks (closure ≤ 0.35·diag, path ≥ 1.4·diag, min 60×30).
- `src/touch.rs` — cyttsp5, Y inverted. Tap(2)=mirror undo, Tap(4)=page op,
  1-finger horizontal ≥150 px = page turn → shadow.clear(). Palm-gated by
  pen proximity (suppress()).
- `src/evdev.rs` — `input_event` is **16 bytes on 32-bit ARM**, 24 on 64-bit
  hosts; parse + encode share EV_SIZE.
- `src/oracle.rs` — non-streaming chat-completions; grok via auth.rs OAuth
  (same store as riddle — **`scribe-auth.json`, falls back to
  `riddle-auth.json`**, either next to the binary). The xAI refresh MUST echo
  scope/plan/referrer (see auth.rs) or the tier claim drops → 429 0/0.
- `src/script.rs` — text → Dancing Script → Zhang-Suen thin → traced strokes;
  `layout()` wraps and places at page coords.

Daemon-side invariants: only one op in flight; replies apply only while the
pen is out of proximity; after any injection call `pen.discard()` so scribe
doesn't read its own synthetic strokes as user ink (written strokes are added
to the shadow via `add_synthetic` instead).

## Building

Cross-compile (static musl, no SDK); the binary is Unix-only:

```sh
docker run --rm -v "$PWD":/home/rust/src messense/rust-musl-cross:armv7-musleabihf \
    cargo build --release
./scripts/make-bundle.sh    # stages dist/scribe/
```

`cargo test` runs on ANY host including Windows — device-only modules
(touch, inject, daemon, PenDevice) are `#[cfg(unix)]`-gated; the logic
modules (trigger, shadow, script, oracle parsing, mapping) are portable and
tested.

## Deploying

`/home/root/scribe/` on the tablet: binary, `scribe.service`,
`oracle.env` (optional), auth json. `install-on-device.sh` installs/enables
the systemd unit. `/home/root` survives OS updates; `/etc/systemd` does not —
re-run the installer after updates. Logs: `journalctl -fu scribe`.

Bring-up order on a new device: `--observe` (verify mapping + trigger), then
`--inject-test`, `--erase-test` (verify xochitl accepts injected events and
the eraser passes cover ink), then the real loop. Stop the service before
running a second copy.

## Hardware-verified gotchas (2026-07 bring-up)

- **xochitl discards strokes that START in the left edge band (x ≲ 100).**
  Silent — the write succeeds, nothing inks. Cost hours. All injected ink
  must begin at x ≥ ~120 (`MARGIN`, and X0 in `draw_divider`).
- Injecting faster than the digitizer (~570+ frames/s sustained) risks
  xochitl dropping tool transitions → it erases fresh ink ("ghosting").
  Current: 140 Hz frames, speed via spatial step (SCRIBE_FRAME_HZ /
  SCRIBE_INK_SPEED / SCRIBE_ERASE_SPEED).
- Our own reader always overflows during injection (SYN_DROPPED) —
  pen.rs resyncs via EVIOCGKEY; without it proximity wedges true forever.

## Known unknowns (verify on hardware, adjust here)

- Injection pacing (SCRIBE_INK_PPS=700) and whether xochitl drops frames at
  higher rates.
- xochitl rubber brush radius — erase passes assume ≥ ~6 px coverage; if ink
  ghosts survive, widen the offsets in `inject.rs::erase_path`.
- Whether newer-firmware touch reports need `pt_mt` name matching (handled)
  or different Y ranges.
- 4-finger tap may collide with palm+fingers on some grips; raise to 5 if
  false triggers occur (riddle uses 5 for quit — fine, riddle grabs input
  while open so the two never see gestures at the same time).
