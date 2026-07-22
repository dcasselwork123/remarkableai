//! The daemon: watch the pen and touch streams alongside xochitl, keep the
//! shadow page, detect triggers, and run circle → oracle → erase → rewrite.

use std::sync::mpsc;
use std::time::Instant;

use ab_glyph::FontRef;

use crate::inject::Injector;
use crate::oracle::Oracle;
use crate::pen::{PenDevice, Tool};
use crate::screen::{BBox, SCREEN_H, SCREEN_W};
use crate::shadow::{Shadow, Stroke};
use crate::touch::{Gesture, TouchDevice};
use crate::trigger;

/// Radius (px) the shadow forgets around a real-eraser sample. Roughly
/// xochitl's rubber brush.
const USER_ERASE_R: i32 = 12;
/// Left/right page margin for laid-out replies. Must clear xochitl's left
/// edge-rejection band (~100px): strokes STARTING inside it are discarded.
const MARGIN: i32 = 120;

// Print-style hand: far more legible than a cursive for the "clean up my
// handwriting" use case. Override with SCRIBE_FONT (e.g. the bundled
// DancingScript.ttf for the flowing look).
const FONT_BYTES: &[u8] = include_bytes!("../fonts/PatrickHand-Regular.ttf");

struct PendingOp {
    /// The circle's interior — drawings fill this.
    region: BBox,
    /// Tight bounds of the circled ink itself — text replies anchor here,
    /// not at the circle's (much larger) box.
    ink: BBox,
    /// Target strokes (by id) — erased and replaced when the reply arrives.
    ids: Vec<u64>,
    /// Instruction ink in the command zone — consumed (erased) on success.
    zone_ids: Vec<u64>,
    epoch: u64,
    rx: mpsc::Receiver<Result<String, String>>,
}

/// The command zone: everything below this line is instruction ink, never
/// content. Lower third of the page by default.
fn zone_bbox() -> BBox {
    let y = std::env::var("SCRIBE_ZONE_Y").ok().and_then(|v| v.parse().ok()).unwrap_or(1435);
    BBox { x0: 0, y0: y, x1: SCREEN_W as i32 - 1, y1: SCREEN_H as i32 - 1 }
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    load_env_file();

    match args.first().map(String::as_str) {
        Some("--oracle-test") => {
            let path = args.get(1).expect("usage: scribe --oracle-test page.png");
            let png = std::fs::read(path).expect("read png");
            let oracle = Oracle::from_env().expect("oracle setup");
            match oracle.ask(&png) {
                Ok(t) => println!("--- oracle reply ---\n{t}"),
                Err(e) => {
                    eprintln!("oracle error: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--inject-test") => {
            let text = args.get(1).cloned().unwrap_or_else(|| "scribe was here".into());
            let x: i32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(120);
            let y: i32 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(200);
            let pen = PenDevice::open_shared().expect("pen device");
            let mut inj = Injector::open(pen.path()).expect("injector");
            let font = load_font();
            eprintln!("scribe: injecting test text at ({x},{y}) in 3s — open a notebook page…");
            std::thread::sleep(std::time::Duration::from_secs(3));
            for stroke in crate::script::layout(&font, &text, text_px(), (x, y), 1100) {
                inj.pen_stroke(&stroke).expect("inject");
            }
            eprintln!("scribe: done");
            return;
        }
        Some("--probe-test") => {
            let pen = PenDevice::open_shared().expect("pen device");
            let mut inj = Injector::open(pen.path()).expect("injector");
            let font = load_font();
            eprintln!("scribe: drawing 5 probes in 3s — watch which ones appear…");
            std::thread::sleep(std::time::Duration::from_secs(3));
            let wobble = |x0: i32, y: i32, x1: i32| -> Vec<(i32, i32)> {
                (0..)
                    .map(|i| x0 + i * 12)
                    .take_while(|x| *x <= x1)
                    .map(|x| (x, y + ((x as f32 / 55.0).sin() * 2.5) as i32))
                    .collect()
            };
            let pause = || std::thread::sleep(std::time::Duration::from_millis(600));
            eprintln!("scribe: probe 1 — long line at y=700");
            inj.pen_stroke(&wobble(40, 700, 1364)).expect("p1");
            pause();
            eprintln!("scribe: probe 2 — short line at y=900");
            inj.pen_stroke(&wobble(40, 900, 440)).expect("p2");
            pause();
            eprintln!("scribe: probe 3 — small AI label at (40,1000)");
            for s in crate::script::layout(&font, "AI", 30.0, (40, 1000), 200) {
                inj.pen_stroke(&s).expect("p3");
            }
            pause();
            eprintln!("scribe: probe 4 — big AI label at (400,1000)");
            for s in crate::script::layout(&font, "AI", 60.0, (400, 1000), 300) {
                inj.pen_stroke(&s).expect("p4");
            }
            pause();
            eprintln!("scribe: probe 5 — long line at y=1248");
            inj.pen_stroke(&wobble(40, 1248, 1364)).expect("p5");
            eprintln!("scribe: done — which probes are on the page?");
            return;
        }
        Some("--divider-test") => {
            let pen = PenDevice::open_shared().expect("pen device");
            let mut inj = Injector::open(pen.path()).expect("injector");
            let font = load_font();
            eprintln!("scribe: drawing the zone divider in 3s — open a notebook page…");
            std::thread::sleep(std::time::Duration::from_secs(3));
            match draw_divider(&mut inj, &font) {
                Ok(strokes) => eprintln!("scribe: divider drawn ({} strokes)", strokes.len()),
                Err(e) => eprintln!("scribe: divider failed: {e}"),
            }
            return;
        }
        Some("--erase-test") => {
            let nums: Vec<i32> = args[1..].iter().filter_map(|s| s.parse().ok()).collect();
            let [x0, y0, x1, y1] = nums[..] else {
                eprintln!("usage: scribe --erase-test x0 y0 x1 y1  (screen px)");
                std::process::exit(2);
            };
            let pen = PenDevice::open_shared().expect("pen device");
            let mut inj = Injector::open(pen.path()).expect("injector");
            eprintln!("scribe: erasing ({x0},{y0})-({x1},{y1}) in 3s…");
            std::thread::sleep(std::time::Duration::from_secs(3));
            inj.erase_area(x0, y0, x1, y1).expect("erase");
            return;
        }
        Some("--observe") => daemon_loop(true),
        None => daemon_loop(false),
        Some(other) => {
            eprintln!(
                "unknown argument {other}\nusage: scribe [--observe | --oracle-test png | \
                 --inject-test \"text\" | --erase-test x0 y0 x1 y1]"
            );
            std::process::exit(2);
        }
    }
}

fn daemon_loop(observe_only: bool) {
    let mut pen = PenDevice::open_shared().expect("pen device");
    let mut touch = TouchDevice::open_shared().expect("touch device");
    let mut injector = if observe_only {
        None
    } else {
        Some(Injector::open(pen.path()).expect("injector"))
    };
    let oracle = match Oracle::from_env() {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("scribe: WARNING: {e}");
            eprintln!("scribe: running without an oracle — triggers will ink an error note");
            None
        }
    };
    let font = load_font();
    let cfg = trigger::Config::default();
    let t0 = Instant::now();
    let mut shadow = Shadow::new();
    let mut pending: Option<PendingOp> = None;
    // A finished reply waiting for the pen to move away before applying.
    let mut ready: Option<(PendingOp, Result<String, String>)> = None;
    let mut was_touching = false;
    // Scribe's own zone-divider ink (5-finger tap) — never content, never
    // instruction, never sent to the oracle.
    let mut divider_ids: Vec<u64> = Vec::new();

    eprintln!(
        "scribe: watching (hold {}ms; {} mode)",
        cfg.hold_ms,
        if observe_only { "observe-only" } else { "active" }
    );

    loop {
        wait_input(&pen, &touch, 20);
        let now_ms = t0.elapsed().as_millis() as u64;

        // ---- pen ----
        let samples = pen.drain();
        let mut stroke_done = false;
        for s in &samples {
            if s.proximity {
                // Palm rejection: while the pen is near the glass, finger
                // contacts are the writing hand, not gestures.
                touch.suppress();
            }
            match (s.tool, s.touching) {
                (Tool::Pen, true) => shadow.pen_point(s.x, s.y, now_ms),
                (Tool::Eraser, true) => shadow.erase_point(s.x, s.y, USER_ERASE_R),
                (_, false) => {
                    if was_touching {
                        stroke_done = true;
                    }
                }
            }
            was_touching = s.touching;
        }

        if stroke_done {
            if let Some(id) = shadow.pen_up() {
                let stroke = shadow.get(id).unwrap();
                if let Some(region) = trigger::detect(stroke, &cfg) {
                    eprintln!(
                        "scribe: TRIGGER circle+hold region=({},{})-({},{})",
                        region.x0, region.y0, region.x1, region.y1
                    );
                    if observe_only {
                        eprintln!("scribe: (observe mode: not acting)");
                    } else if pending.is_some() || ready.is_some() {
                        eprintln!("scribe: an operation is already in flight — ignoring");
                    } else {
                        let circle = shadow.take(id).unwrap();
                        let zone = zone_bbox();
                        let has_instruction = shadow
                            .ids_in(&zone)
                            .iter()
                            .any(|i| !divider_ids.contains(i));
                        if shadow.ids_in(&region).is_empty() && !has_instruction {
                            // No target ink AND no instruction — probably a
                            // circle around ink from before the daemon
                            // started. Leave it alone. (An EMPTY circle plus
                            // a zone instruction is valid: "draw a car" +
                            // circle where it should go.)
                            eprintln!(
                                "scribe: empty circle and empty command zone — leaving it \
                                 untouched"
                            );
                            let now_ms = t0.elapsed().as_millis() as u64;
                            shadow.add_synthetic(circle.pts, now_ms);
                        }
                        // Injecting while the real pen hovers would weave the
                        // user's coordinates into the synthetic stroke — wait
                        // for the hand to pull back first.
                        else if settle_pen(&mut pen, &mut touch, &mut shadow, t0, 30_000) {
                            // Rasterize (with the circle in the image) BEFORE
                            // erasing it off the glass.
                            pending =
                                start_op(&shadow, region, Some(&circle), &divider_ids, &oracle);
                            if pending.is_some() {
                                if let Some(inj) = injector.as_mut() {
                                    // Erase the circle: the visible ack.
                                    if let Err(e) = inj.erase_path(&circle.pts) {
                                        eprintln!("scribe: erase failed: {e}");
                                    }
                                    pen.discard();
                                }
                            } else {
                                // Op never started — leave the circle on the
                                // page and put it back in the shadow.
                                let now_ms = t0.elapsed().as_millis() as u64;
                                shadow.add_synthetic(circle.pts, now_ms);
                            }
                        } else {
                            eprintln!("scribe: pen never moved away — abandoning trigger");
                            let now_ms = t0.elapsed().as_millis() as u64;
                            shadow.add_synthetic(circle.pts, now_ms);
                        }
                    }
                }
            }
        }

        // ---- touch gestures ----
        if !pen.proximity() {
            for g in touch.drain() {
                match g {
                    Gesture::Tap(2) => {
                        // xochitl undid the newest stroke; mirror it.
                        if let Some(id) = shadow.undo_pop() {
                            divider_ids.retain(|d| *d != id);
                        }
                    }
                    Gesture::Tap(4) => {
                        let region = content_region(&shadow, &divider_ids);
                        eprintln!("scribe: TRIGGER four-finger tap (page)");
                        if observe_only {
                            eprintln!("scribe: (observe mode: not acting)");
                        } else if pending.is_some() || ready.is_some() {
                            eprintln!("scribe: an operation is already in flight — ignoring");
                        } else if region.is_empty() {
                            eprintln!("scribe: no content ink on the shadow page — nothing to do");
                        } else {
                            pending = start_op(&shadow, region, None, &divider_ids, &oracle);
                        }
                    }
                    Gesture::Tap(5) => {
                        eprintln!("scribe: five-finger tap — drawing the command-zone divider");
                        if observe_only {
                            eprintln!("scribe: (observe mode: not acting)");
                        } else if pending.is_some() || ready.is_some() {
                            eprintln!("scribe: an operation is already in flight — ignoring");
                        } else if settle_pen(&mut pen, &mut touch, &mut shadow, t0, 10_000) {
                            if let Some(inj) = injector.as_mut() {
                                // Give xochitl a beat after the multi-touch
                                // contact before pen events arrive.
                                std::thread::sleep(std::time::Duration::from_millis(1500));
                                let now_ms = t0.elapsed().as_millis() as u64;
                                match draw_divider(inj, &font) {
                                    Ok(strokes) => {
                                        pen.discard();
                                        for s in strokes {
                                            divider_ids.push(shadow.add_synthetic(s, now_ms));
                                        }
                                        eprintln!("scribe: divider drawn");
                                    }
                                    Err(e) => eprintln!("scribe: divider failed: {e}"),
                                }
                            }
                        } else {
                            eprintln!("scribe: pen stayed near the glass — divider not drawn");
                        }
                    }
                    Gesture::PageSwipe => {
                        // Page turned: everything scribe knew is now stale.
                        eprintln!("scribe: page swipe — clearing shadow ({} strokes)", shadow.len());
                        shadow.clear();
                        divider_ids.clear();
                    }
                    _ => {}
                }
            }
        }

        // ---- oracle replies ----
        if let Some(op) = pending.take() {
            match op.rx.try_recv() {
                Ok(result) => {
                    eprintln!("scribe: reply ready — waiting for the pen to move away");
                    ready = Some((op, result));
                }
                Err(mpsc::TryRecvError::Empty) => pending = Some(op),
                Err(mpsc::TryRecvError::Disconnected) => {
                    ready = Some((op, Err("oracle thread died".into())))
                }
            }
        }

        // Apply a finished reply only while the pen is away — never fight
        // the user's hand for the page.
        if ready.is_some() && !pen.proximity() {
            // Require a quiet 200ms (drains stragglers); if the pen comes
            // back, keep waiting and retry next loop.
            if settle_pen(&mut pen, &mut touch, &mut shadow, t0, 5_000) {
                let (op, result) = ready.take().unwrap();
                if op.epoch != shadow.epoch {
                    eprintln!("scribe: page changed while thinking — dropping reply");
                    continue;
                }
                if let Some(inj) = injector.as_mut() {
                    apply_reply(inj, &mut pen, &mut shadow, &font, &op, result, t0);
                    pen.discard();
                }
            }
        }
    }
}

/// Draw the command-zone divider (rule + "AI" label); returns the strokes
/// for shadow bookkeeping. Shared by the 5-finger gesture and --divider-test.
fn draw_divider(
    inj: &mut Injector,
    font: &FontRef,
) -> std::io::Result<Vec<Vec<(i32, i32)>>> {
    let y = zone_bbox().y0;
    // Strokes that BEGIN in xochitl's left edge band (x ≲ 100) are silently
    // discarded (palm/edge rejection) — keep the divider inboard of it.
    // The wobble keeps the coordinate stream looking like a real pen.
    const X0: i32 = 130;
    const X1: i32 = 1290;
    let line: Vec<(i32, i32)> = (0..)
        .map(|i| X0 + i * 12)
        .take_while(|x| *x <= X1)
        .map(|x| (x, y + ((x as f32 / 55.0).sin() * 2.5) as i32))
        .collect();
    let mut strokes = vec![line];
    strokes.extend(crate::script::layout(font, "AI", 45.0, (X0, y + 10), 200));
    for s in &strokes {
        inj.pen_stroke(s)?;
    }
    Ok(strokes)
}

/// Bounds of the content ink: everything except the command zone and
/// scribe's own divider — what a four-finger page action targets.
fn content_region(shadow: &Shadow, divider_ids: &[u64]) -> BBox {
    let zone = zone_bbox();
    let zone_ids = shadow.ids_in(&zone);
    let ids: Vec<u64> = shadow
        .all_ids()
        .into_iter()
        .filter(|i| !zone_ids.contains(i) && !divider_ids.contains(i))
        .collect();
    shadow.bbox_of(&ids, 10)
}

/// Rasterize the page (the circle drawn in so the oracle can see what's
/// marked, the command zone below the synthetic rule), hand it to the
/// oracle on a worker thread. Only the TARGET strokes — inside `region`,
/// above the zone — get erased and replaced when the reply lands; the zone
/// ink that drove the operation is consumed on success.
fn start_op(
    shadow: &Shadow,
    region: BBox,
    circle: Option<&Stroke>,
    divider_ids: &[u64],
    oracle: &Option<Oracle>,
) -> Option<PendingOp> {
    let zone = zone_bbox();
    let zone_ids: Vec<u64> = shadow
        .ids_in(&zone)
        .into_iter()
        .filter(|i| !divider_ids.contains(i))
        .collect();
    // Targets: circled ink, minus instruction ink and scribe's own divider.
    let ids: Vec<u64> = shadow
        .ids_in(&region)
        .into_iter()
        .filter(|i| !zone_ids.contains(i) && !divider_ids.contains(i))
        .collect();
    if ids.is_empty() && zone_ids.is_empty() {
        eprintln!("scribe: no target ink and no instruction — skipping");
        return None;
    }
    // The oracle sees the whole known page (minus the real divider ink) with
    // a clean synthetic rule at the zone boundary, so content vs instruction
    // is unambiguous regardless of what the user's divider looks like.
    let all: Vec<u64> =
        shadow.all_ids().into_iter().filter(|i| !divider_ids.contains(i)).collect();
    let mut crop = shadow.bbox_all(10);
    if let Some(c) = circle {
        for &(x, y) in &c.pts {
            crop.add(x, y, 10);
        }
    }
    crop.add(0, zone.y0, 10);
    crop.add(SCREEN_W as i32 - 1, zone.y0, 10);
    let rule = Stroke {
        pts: vec![(0, zone.y0), (SCREEN_W as i32 - 1, zone.y0)],
        ms: vec![0, 0],
    };
    let mut extras: Vec<&Stroke> = vec![&rule];
    if let Some(c) = circle {
        extras.push(c);
    }
    let png = match shadow.rasterize_png(&crop, &all, &extras) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("scribe: rasterize failed: {e}");
            return None;
        }
    };
    eprintln!(
        "scribe: asking oracle ({} target / {} instruction / {} total strokes, {}b png)",
        ids.len(),
        zone_ids.len(),
        all.len(),
        png.len()
    );
    let ink = shadow.bbox_of(&ids, 0);
    let (tx, rx) = mpsc::channel();
    match oracle {
        Some(o) => {
            let o = o.clone();
            std::thread::spawn(move || {
                let _ = tx.send(o.ask(&png).map_err(|e| e.to_string()));
            });
        }
        None => {
            let _ = tx.send(Err("no oracle configured (see oracle.env)".into()));
        }
    }
    Some(PendingOp { region, ink, ids, zone_ids, epoch: shadow.epoch, rx })
}

/// Erase the circled strokes and write the reply (or a short error note).
/// `pen.discard()` between injections keeps our loopback from overflowing
/// the reader buffer (SYN_DROPPED) mid-operation.
fn apply_reply(
    inj: &mut Injector,
    pen: &mut PenDevice,
    shadow: &mut Shadow,
    font: &FontRef,
    op: &PendingOp,
    result: Result<String, String>,
    t0: Instant,
) {
    let text = match result {
        Ok(t) => t,
        Err(e) => {
            eprintln!("scribe: oracle error: {e}");
            // Ink a small note under the region instead of silence.
            let note = format!("[scribe: {}]", clip(&e, 60));
            let origin = (op.region.x0, op.region.y1 + 10);
            let note_w = (SCREEN_W as i32 - MARGIN - origin.0).max(240);
            for stroke in crate::script::layout(font, &note, 28.0, origin, note_w) {
                let _ = inj.pen_stroke(&stroke);
                pen.discard();
            }
            return;
        }
    };
    eprintln!("scribe: reply: {}", clip(&text, 120));

    // Erase the target strokes, then consume the instruction ink that
    // drove the operation — both by exact retrace, only if they still exist.
    for id in op.ids.iter().chain(op.zone_ids.iter()) {
        if let Some(stroke) = shadow.take(*id) {
            if let Err(e) = inj.erase_path(&stroke.pts) {
                eprintln!("scribe: erase failed: {e}");
            }
            pen.discard();
        }
    }

    // A DRAW reply: render polylines scaled into the region instead of text.
    if let Some(polys) = crate::drawing::parse(&text) {
        let strokes = crate::drawing::place(&polys, &op.region);
        eprintln!("scribe: drawing {} strokes", strokes.len());
        let now_ms = t0.elapsed().as_millis() as u64;
        for stroke in strokes {
            if let Err(e) = inj.pen_stroke(&stroke) {
                eprintln!("scribe: draw failed: {e}");
                break;
            }
            pen.discard();
            shadow.add_synthetic(stroke, now_ms);
        }
        eprintln!("scribe: done");
        return;
    }

    // Write the reply where the circled ink was — or, for an empty circle
    // ("draw/write something HERE"), inside the circle itself.
    let anchor =
        if op.ink.is_empty() { (op.region.x0, op.region.y0 + 20) } else { (op.ink.x0, op.ink.y0) };
    let origin = (anchor.0.max(MARGIN), anchor.1.max(20));
    let max_w = (SCREEN_W as i32 - MARGIN - origin.0).max(240);
    let px = text_px();
    let bold = std::env::var("SCRIBE_BOLD").map(|v| v != "0").unwrap_or(true);
    eprintln!("scribe: writing at {px:.0}px{}", if bold { " bold" } else { "" });
    let now_ms = t0.elapsed().as_millis() as u64;
    for stroke in crate::script::layout(font, &text, px, origin, max_w) {
        if let Err(e) = inj.pen_stroke(&stroke) {
            eprintln!("scribe: write failed: {e}");
            break;
        }
        if bold {
            // Second pass 1px offset: a 2px-wide line instead of a hairline.
            let shifted: Vec<(i32, i32)> = stroke.iter().map(|&(x, y)| (x + 1, y + 1)).collect();
            let _ = inj.pen_stroke(&shifted);
        }
        pen.discard();
        // Track our own ink so a follow-up circle can target it.
        shadow.add_synthetic(stroke, now_ms);
    }
    eprintln!("scribe: done");
}

/// Block until the pen has been out of proximity for a quiet 200 ms, feeding
/// user strokes into the shadow meanwhile (no trigger detection — we're mid
/// operation). Returns false if the pen never cleared within `max_ms`.
fn settle_pen(
    pen: &mut PenDevice,
    touch: &mut TouchDevice,
    shadow: &mut Shadow,
    t0: Instant,
    max_ms: u64,
) -> bool {
    let start = Instant::now();
    let mut clear_since: Option<Instant> = None;
    loop {
        let now_ms = t0.elapsed().as_millis() as u64;
        for s in pen.drain() {
            if s.proximity {
                touch.suppress();
            }
            match (s.tool, s.touching) {
                (Tool::Pen, true) => shadow.pen_point(s.x, s.y, now_ms),
                (Tool::Eraser, true) => shadow.erase_point(s.x, s.y, USER_ERASE_R),
                (_, false) => {
                    shadow.pen_up();
                }
            }
        }
        for g in touch.drain() {
            match g {
                Gesture::Tap(2) => {
                    shadow.undo_pop();
                }
                Gesture::PageSwipe => {
                    eprintln!("scribe: page swipe — clearing shadow ({} strokes)", shadow.len());
                    shadow.clear();
                }
                _ => {}
            }
        }
        if pen.proximity() {
            clear_since = None;
        } else {
            match clear_since {
                Some(cs) if cs.elapsed().as_millis() >= 200 => return true,
                Some(_) => {}
                None => clear_since = Some(Instant::now()),
            }
        }
        if start.elapsed().as_millis() as u64 > max_ms {
            return false;
        }
        wait_input(pen, touch, 20);
    }
}

/// Reply handwriting size (px line height of the glyphs themselves).
fn text_px() -> f32 {
    std::env::var("SCRIBE_TEXT_PX").ok().and_then(|v| v.parse().ok()).unwrap_or(112.0)
}

fn load_font() -> FontRef<'static> {
    if let Ok(path) = std::env::var("SCRIBE_FONT") {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
                match FontRef::try_from_slice(leaked) {
                    Ok(f) => {
                        eprintln!("scribe: font {path}");
                        return f;
                    }
                    Err(e) => eprintln!("scribe: bad SCRIBE_FONT {path} ({e}); using built-in"),
                }
            }
            Err(e) => eprintln!("scribe: cannot read SCRIBE_FONT {path} ({e}); using built-in"),
        }
    }
    FontRef::try_from_slice(FONT_BYTES).expect("built-in font")
}

/// Load oracle.env (KEY=value lines) from next to the binary, like riddle.
fn load_env_file() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let path = dir.join("oracle.env");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if std::env::var_os(k.trim()).is_none() {
                std::env::set_var(k.trim(), v);
            }
        }
    }
    eprintln!("scribe: loaded {}", path.display());
}

/// Block until the pen or touch fd is readable, or `ms` elapsed.
fn wait_input(pen: &PenDevice, touch: &TouchDevice, ms: i32) {
    let mut fds = [
        libc::pollfd { fd: pen.raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: touch.raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    unsafe {
        libc::poll(fds.as_mut_ptr(), fds.len() as _, ms);
    }
}

fn clip(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
