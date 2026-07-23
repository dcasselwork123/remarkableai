# CLAUDE.md

## What this is

**scribe** — a background daemon giving the **reMarkable 2's stock notebook
app (xochitl)** AI abilities. The user writes normally; a **command zone** at
the bottom of the page holds handwritten instructions; **circling ink and
holding the pen still ~1 s** sends the page to a vision LLM and the result is
written back **as real notebook ink** (injected pen events — saves, syncs,
undoes like the user's own). Working end-to-end on hardware since 2026-07-22.

Sibling of riddle (a takeover/qtfb *app*; scribe instead rides alongside
xochitl) — fresh checkout at `D:\Dev\RemarkableDiary` (has `riddle-login`,
`src/bin/riddle-login.rs`; the copy at `D:\Dev\RemarkableApp\Riddle` is stale).
Reuses riddle's pen mapping, subscription OAuth, and handwriting synthesis.
Repo: https://github.com/dcasselwork123/remarkableai (public).

## The user experience

- **5-finger tap** → scribe draws the zone divider (wobbly rule + "AI"
  label) at `SCRIBE_ZONE_Y` (default 1435). Purely visual — the zone is
  geometric and works without it.
- Ink **below** the zone line = instruction ("translate to Afrikaans",
  "draw a car"). Ink above = content.
- **Circle content + hold ~1 s + lift pen away** → circle erases (ack),
  region+page go to the oracle, target ink is erased and replaced by the
  reply; the zone instruction erases itself on success (consumed).
- **Circle blank space** + zone instruction → the circle is a canvas: the
  output (text or drawing) is placed there, scaled to fit.
- Empty zone → circling is a "make this legible" rewrite.
- **4-finger tap** → whole-page op (all content above the line as target).
- **2-finger tap** = xochitl undo (mirrored into the shadow). 1-finger
  horizontal swipe = page turn → shadow resets.
- DRAW replies (model outputs polylines) render as pen-drawn shapes.

## Architecture

```
pen evdev (SHARED — never EVIOCGRAB) ──► daemon.rs loop ──► shadow.rs (stroke mirror, stable ids)
touch evdev (shared)  ─ gestures ────────┘   │ pen-up: trigger.rs (circle+hold?)
                                             ▼
        shadow.rasterize_png(whole page + synthetic zone rule + the circle)
                                             ▼ oracle.rs (blocking, worker thread)
        inject.rs: erase_path per target/zone stroke (BTN_TOOL_RUBBER),
        then script.rs layout → pen strokes (BTN_TOOL_PEN) or drawing.rs
        polylines, written into /dev/input/event1 — xochitl inks them
```

- `src/pen.rs` — rM2 Wacom mapping (rotated 90°) + `to_raw` inverse for the
  injector. **Resyncs via EVIOCGKEY on SYN_DROPPED** — without this,
  proximity wedges true forever after injection floods the reader.
- `src/touch.rs` — cyttsp5 (Y inverted). Tap = **per-finger** travel <
  60 px (never sum finger jitter — a 5-finger tap can't pass a summed
  threshold). Gestures palm-gated by pen proximity (`suppress()`).
- `src/shadow.rs` — stroke store with stable ids; ops erase *by id* so user
  edits during the oracle wait can't shift targets. Single-sample tap dots
  ARE committed (periods/i-dots must be erasable). `ids_in` uses a loose
  **30%-points-inside** test so a sloppy circle that clips a letter still
  captures it. `epoch` bumps on page swipe; stale replies are dropped.
- `src/trigger.rs` — timing-first: motionless ≥`SCRIBE_HOLD_MS` (900) tail,
  then crude loop checks (closure ≤ 0.35·diag, path ≥ 1.4·diag, ≥60×30 px).
- `src/inject.rs` — event injection. **140–500 Hz frames; speed comes from
  spatial step, not frame rate** (`step = speed/hz`, capped 40). Erase =
  decimated waypoints (≤8 px), **1–5 offset passes** (`SCRIBE_ERASE_PASSES`,
  default 5; 3 verified visually clean on-device) alternating direction
  (continuous path — a jump would erase a line through unrelated ink) +
  scrub circles at the drag's true endpoints (start point after an even pass
  count). 30 ms pauses at every tool transition (a dropped transition leaves
  xochitl using the wrong tool → "ghosting").
- `src/oracle.rs` — three backends behind one blocking `ask`. (a) Grok via
  auth.rs OAuth (same store as riddle: `scribe-auth.json`, falls back to
  `riddle-auth.json`, or `SCRIBE_AUTH_FILE`) on `/chat/completions`; the xAI
  refresh MUST echo scope/plan/referrer (auth.rs) or the tier claim drops →
  429 0/0. (b) any OpenAI-compatible key. (c) ChatGPT subscription (Plus/Pro,
  provider `chatgpt` auth file from `riddle-login chatgpt`): the Codex OAuth
  client's Responses dialect at `chatgpt.com/backend-api/codex/responses`,
  store=false + streamed SSE accumulated to one reply — ported from riddle's
  CodexOracle, **compiles but untested end-to-end (no account)**. The
  SYSTEM_PROMPT defines the two-section page contract + DRAW protocol.
- `src/drawing.rs` — DRAW reply parser (0–1000 space polylines) + placement
  (aspect-preserving, centered in the circle).
- `src/script.rs` — text → font glyphs → Zhang-Suen thin → traced strokes;
  `layout()` wraps/places. Default hand: Patrick Hand (print, legible);
  Dancing Script ships for `SCRIBE_FONT`.
- `src/evdev.rs` — `input_event` is **16 bytes on 32-bit ARM**, 24 on
  64-bit hosts; parse + encode share EV_SIZE.

Daemon invariants: one op in flight; **never inject while the real pen is in
proximity** (`settle_pen` — interleaved real coords corrupt synthetic
strokes); after any injection `pen.discard()`; injected ink enters the
shadow via `add_synthetic` (divider ink tracked in `divider_ids` — never
target, never instruction, excluded from oracle images).

## Hardware-verified gotchas (cost real debugging hours — do not relearn)

1. **xochitl silently discards strokes that START in the left edge band
   (x ≲ 100).** The write succeeds, nothing inks, no error anywhere. ALL
   injected ink must begin at x ≥ ~120 (`MARGIN`, `X0` in draw_divider).
   Diagnosed via `--probe-test` after every other theory failed.
2. **SYN_DROPPED is guaranteed** during injection (our own loopback
   overflows our reader). Handle it (EVIOCGKEY resync) or pen state wedges.
3. Sustained injection above ~570 frames/s risks xochitl missing tool
   transitions → it erases fresh ink. Keep frame rate real-pen-like; get
   speed from step size. Pauses at tool transitions are load-bearing.
4. Erasing must retrace **decimated** paths — raw handwriting carries
   hundreds of samples/stroke and paced per-sample retracing takes minutes.
5. A user's quick tap = 1-sample stroke; if the shadow drops it, dots
   survive every erase.
6. The injected-ink appearance depends on the user's currently selected
   xochitl tool (ballpoint/marker look good; thin fineliner looks wispy).

## Building & tests

```sh
docker run --rm -v "$PWD":/home/rust/src messense/rust-musl-cross:armv7-musleabihf \
    cargo build --release
```

`cargo test` runs on ANY host incl. Windows (24 tests) — device-only modules
(touch, inject, daemon, PenDevice) are `#[cfg(unix)]`-gated; trigger, shadow,
drawing, script, oracle parsing, and the coordinate mappings are portable.
On Windows, docker path syntax: `-v "C:\Dev\remarkableapp2:/home/rust/src"`
via PowerShell. `scripts/make-bundle.sh` needs a POSIX shell (Git Bash).
**Line endings**: anything the tablet parses (`*.sh`, systemd units) must be
LF — a CRLF checkout breaks BusyBox `sh` with `set: -: invalid option`.
`.gitattributes` forces LF; if a script misbehaves on-device, check for `\r`
first (`sed -i 's/\r$//'` fixes it in place).

## Deploying (the actual dev loop used)

Tablet at `10.11.99.1` over USB, SSH key auth already set up. Install dir
`/home/root/scribe/` (systemd unit `scribe.service`, installed by
`install-on-device.sh`; survives reboots). **OS updates are survived
automatically** since 2026-07-23: `scripts/persist/` (ported from riddle's
xovi-persist) injects scribe's units into a staged A/B update's rootfs via a
5-min timer + shutdown hook — the persist units re-persist themselves too.
If it ever misses, re-run the installer. Iteration loop:

```powershell
Copy-Item target\armv7-unknown-linux-musleabihf\release\scribe dist\scribe\scribe -Force
scp -O dist/scribe/scribe root@10.11.99.1:/home/root/scribe/scribe.new
ssh root@10.11.99.1 "systemctl stop scribe; mv /home/root/scribe/scribe.new /home/root/scribe/scribe; chmod +x /home/root/scribe/scribe; systemctl start scribe"
```

Live logs: `ssh root@10.11.99.1 journalctl -fu scribe` (run in background
during user testing — every daemon decision is logged and this is how every
bug was found). **A service restart wipes the shadow: the user must write
FRESH ink before retesting.** Oracle auth: `oracle.env` sets
`SCRIBE_AUTH_FILE=/home/root/xovi/exthome/appload/riddle/riddle-auth.json`
— shared with riddle on purpose; Grok rotates refresh tokens, so two
independently-refreshing copies would invalidate each other.

## Diagnostics (on device; stop the service first)

```sh
./scribe --observe                    # log triggers/gestures, act on nothing
./scribe --oracle-test page.png       # oracle round-trip, no pen
./scribe --inject-test "text" [x y]   # write text at a position
./scribe --divider-test               # draw the zone divider
./scribe --probe-test                 # 5 varied probes — the injection bisector
./scribe --erase-test x0 y0 x1 y1     # zigzag-erase a rect (erase_area — NOT the real flow)
./scribe --erase-text-test "text" [x y]  # retrace-erase laid-out text — the
                                      # real flow's erase_path, for tuning
                                      # SCRIBE_ERASE_SPEED/PASSES by eye
SCRIBE_DEBUG_PEN=1 ./scribe           # log first 60 pen samples (raw+mapped)
```

## Tuning (oracle.env, no rebuild; restart the service)

`SCRIBE_HOLD_MS` 900 · `SCRIBE_ZONE_Y` 1435 · `SCRIBE_TEXT_PX` 112 ·
`SCRIBE_FRAME_HZ` 500 · `SCRIBE_INK_SPEED` 2000 · `SCRIBE_ERASE_SPEED` 6000 ·
`SCRIBE_ERASE_PASSES` 5 · `SCRIBE_BOLD` 1 · `SCRIBE_FONT` path ·
`SCRIBE_GROK_MODEL` grok-4.3 · `SCRIBE_CHATGPT_MODEL` gpt-5.1 ·
`SCRIBE_OPENAI_KEY/BASE/MODEL` · `SCRIBE_ORACLE` grok|openai|chatgpt ·
`SCRIBE_MAX_TOKENS` 1200 · `SCRIBE_REASONING` (defaults to `low` on chatgpt)

The user's tablet runs values picked by eye in on-device A/B tests
(2026-07-23): `SCRIBE_INK_SPEED=20000`, `SCRIBE_FRAME_HZ=500`,
`SCRIBE_ERASE_SPEED=15000`, `SCRIBE_ERASE_PASSES=3`. These live ONLY in the
tablet's `/home/root/scribe/oracle.env` — never clobber it when redeploying.

## Known limitations / next candidates

- **Vertical in-page scrolling desyncs the shadow** (screen-space model;
  only horizontal page-turn swipes reset it). Next fix: treat significant
  1-finger vertical swipes as shadow-invalidating.
- Shadow only knows ink drawn while the daemon runs, on the current page;
  circling pre-daemon ink is a no-op (guarded: circle left untouched).
- Undo mirroring is 1-level; real-eraser mirroring is approximate (r=12).
- Landscape mode, zoom, PDFs/ebooks: untested/unsupported.
- No release zip/tag on the GitHub repo yet.
