# scribe — AI in the margins of your reMarkable 2

Write in the **normal reMarkable notebook** as usual. The bottom of the page
(below y≈1248, `SCRIBE_ZONE_Y`) is the **command zone** — handwriting there
is always an instruction for the AI, never page content.

- **Tap with five fingers** → scribe draws the zone divider (a line + "AI"
  label) on the current page, so you can see the wall. Optional — the zone
  works with or without the visual line.
- **Write an instruction in the zone** ("translate to German", "fix the
  grammar", "draw this as a diagram"), then **circle the target ink above
  and hold the pen still ~1 second**. The circle vanishes (acknowledgment),
  the instruction runs against the circled ink, the result replaces it as
  *real notebook ink* — and the instruction erases itself, consumed.
- **No instruction in the zone?** Circling + holding rewrites the circled
  handwriting legibly.
- **Tap with four fingers** → same flow for all content on the page.

No takeover, no custom app: xochitl runs untouched the whole time. scribe is
a small background daemon that listens to the pen/touch event streams
(shared, never grabbed), keeps its own shadow copy of your strokes, and
writes back by injecting synthetic pen events into the Wacom digitizer —
xochitl can't tell them from a real pen.

## Requirements

- reMarkable 2 with SSH access (stock — password in Settings → Help → About).
- An oracle: a **Grok subscription login** or a **ChatGPT (Plus/Pro)
  subscription login** (reuse `riddle-auth.json` from the
  [riddle](https://github.com/MaximeRivest/Riddle) project's `riddle-login
  grok` / `riddle-login chatgpt`) **or** any OpenAI-compatible API key for a
  vision model.
- Docker (or `cargo zigbuild`) on your computer to cross-compile.

No xovi/AppLoad needed (harmless if present — when the riddle app grabs the
pen, scribe simply sees nothing until it exits).

## Build & install

```sh
docker run --rm -v "$PWD":/home/rust/src messense/rust-musl-cross:armv7-musleabihf \
    cargo build --release
./scripts/make-bundle.sh
cp /path/to/riddle-auth.json dist/scribe/scribe-auth.json   # or an API key in oracle.env
scp -O -r dist/scribe root@10.11.99.1:/home/root/scribe
ssh root@10.11.99.1 '/home/root/scribe/install-on-device.sh'
```

Watch it think: `ssh root@10.11.99.1 journalctl -fu scribe`

**Already running riddle on the tablet?** Don't copy the auth file twice —
Grok rotates refresh tokens, so two copies refreshing independently will
invalidate each other. Point scribe at riddle's live copy instead: put

```
SCRIBE_AUTH_FILE=/home/root/xovi/exthome/appload/riddle/riddle-auth.json
```

in `/home/root/scribe/oracle.env` and skip the `scribe-auth.json` copy. (If
you're not running riddle on the device, copy the freshest riddle-auth.json
you have — the one on the tablet if it was ever used there.)

An OS update wipes the systemd unit (not `/home/root`): re-run
`install-on-device.sh` afterwards.

## Bring-up / diagnostics (on the device)

```sh
systemctl stop scribe                  # don't run two copies
./scribe --observe                     # log strokes + trigger detections, act on nothing
./scribe --oracle-test page.png        # full oracle round-trip, no pen involved
./scribe --inject-test "hello page"    # writes text into the open notebook after 3s
./scribe --erase-test 100 200 600 400  # erases a rectangle after 3s
```

Tuning knobs live in `oracle.env` (see `oracle.env.example`): hold duration,
handwriting size, animation speed, model, font.

## Ground rules & limitations (v1)

- **Turn OFF "Enable shapes"** in documents where you use scribe — the
  draw-and-hold trigger is the same gesture xochitl's shape-snap uses.
- scribe only knows ink drawn **while it was running, on the current page**.
  After a page turn (detected by the swipe) it starts fresh — circle things
  you wrote since. Pre-existing pages are invisible to it.
- Undo depth: a two-finger tap is mirrored (one stroke); erasing with the
  real eraser is approximated. If the shadow drifts from the page, turn the
  page and come back.
- Don't write while the reply is animating in; your strokes during the
  injection window are ignored by the shadow.
- Highlighter and vertical-swipe scrolled pages: untested; continuous-scroll
  documents will confuse the region mapping.
- Like every rM2 hack: an OS update may stop the daemon (reinstall), and you
  use this at your own risk. Keep SSH working.

## What leaves the device

Only the circled region, rasterized as a small grayscale PNG, sent to the
oracle **you** configured (Grok or your OpenAI-compatible endpoint). No
telemetry, nothing stored off-device by scribe.

## License

MIT. Reply hands: [Patrick Hand](https://fonts.google.com/specimen/Patrick+Hand)
(default; SIL OFL 1.1, `fonts/OFL-PatrickHand.txt`) and
[Dancing Script](https://github.com/googlefonts/DancingScript) (via
`SCRIBE_FONT`; SIL OFL 1.1, `fonts/OFL.txt`). Portions adapted from
[MaximeRivest/Riddle](https://github.com/MaximeRivest/Riddle) (MIT).
