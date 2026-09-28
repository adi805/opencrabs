#![cfg(windows)]
//! Drive a real window on a real Windows desktop, and measure what happened.
//!
//! This is the receipt the input lane exists to produce. `desktop_input_test`
//! and `desktop_input_posted_test` check the arithmetic on any host, but every
//! one of those checks would still pass if `SendInput` were called with a
//! sensible-looking event that Windows delivered somewhere else. Only a real
//! desktop can answer "did the keystroke arrive", and only a real desktop can
//! answer it with a photograph rather than a claim.
//!
//! Four separate jobs, one per claim, so a failure names the claim that broke
//! instead of the last one that ran:
//!
//! 1. a keystroke changes the photographed frame: the character reached an
//!    application and our own capture saw it. This is the end-to-end loop that
//!    makes the whole module worth having, and it uses the capture path as the
//!    sensor, so the two halves cross-check each other.
//! 2. an injected click leaves the pointer at the point that was aimed at: the
//!    0..=65535 normalisation round-trips on the machine that has to agree with
//!    it. A one-pixel error here is invisible in a unit test and lands the click
//!    on the neighbour of every button.
//! 3. a posted click leaves the pointer exactly where it was: the safety
//!    property that makes the non-intrusive route the default, measured instead
//!    of asserted from a comment.
//! 4. the scan-code route puts the same characters on the same screen as the
//!    unicode route. It is a different API contract, not a different spelling of
//!    the same one, and it is the route that applications which poll keys rather
//!    than read characters can see at all. Without a job for it, the fallback
//!    would only be claimed to exist.
//!
//! A photograph carries two claims at once, and the first run of this file
//! learned that the hard way: a flat frame is exactly what "the keystrokes never
//! arrived" looks like, and also exactly what "the capture cannot see this window
//! repaint" looks like, and those two need opposite fixes. So the pixel floor
//! here is measured against a control (two photographs of the same window with
//! nothing typed between them, which prices the caret honestly) instead of
//! against a constant I would have to invent on a machine with no console cell,
//! and the claim that needs no camera at all (type a command, watch the shell
//! rename its own window through `GetWindowTextW`) lives in
//! `desktop_input_exec_test`.
//!
//! Each job drives a console window it opened itself, for the reason the capture
//! dump gives: a hosted runner's desktop is not ours to depend on, and a window
//! we own is one whose contents we can predict.
//!
//! `#[ignore]`d, so the gating slice stays fast and the artifact workflow asks
//! for these by name.
//!
//! Run locally:
//! `cargo test --locked --profile ci --target x86_64-pc-windows-msvc --lib \
//!  input_dump -- --ignored --nocapture`
use crate::desktop::{
    Delivery, MouseButton, ScreenPoint, click_window, cursor_position, inject_click, inject_text,
    inject_text_as_keys, interactive_session,
};
use crate::tests::desktop_input_util::{
    PAINT_SETTLE, capture_stable, lit_pixels, note, spawn_marker_window, stop, wait_for_window,
    write_png,
};

/// The unit of what gets typed, chosen for its pixel count rather than meaning.
///
/// Nothing here is a command: no Enter is ever sent on this route, so the shell
/// keeps it as a half-typed line and throws it away when the window dies.
const TYPED: &str = "ZOOM7-INPUT-RECEIPT-";

/// How many times [`TYPED`] goes into the same prompt in one go.
///
/// Sixty characters still fits the 100-column console this job asks for, and it
/// matters because the receipt below is a ratio against the window's own jitter:
/// the longer the signal, the smaller the multiplier has to be to be sure.
const TYPED_REPEATS: usize = 3;

/// How much further the typed frame must move than the same window moving by
/// itself, and why that is a ratio and not a pixel count.
///
/// The adversary is a blinking caret, and what a caret costs in pixels is a
/// function of font, cell size, and whether the blink happens to fall between
/// the two photographs. None of those are knowable from the Linux box I write
/// this on, so any constant I could type in would be a guess dressed as a
/// threshold. What is knowable: two photographs of the same window with the same
/// delay and no input between them measure exactly what "nothing happened" looks
/// like on this machine, caret included. Requiring eight times that is a claim
/// about the window's behaviour, not about my arithmetic.
const SIGNAL_OVER_NOISE: usize = 8;

/// The floor when the window turns out to be perfectly still between two
/// photographs, because a ratio against zero would accept one stray pixel.
/// Sixty-odd characters is several console cells of light; this is only there to
/// stop a noiseless window and a single antialiasing difference counting.
const MIN_SIGNAL_PIXELS: usize = 120;

/// 1. A keystroke reaches the window and our own capture sees it.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_a_keystroke_changes_the_captured_frame() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no desktop to type into");
        return;
    }
    let marker = format!("OCIN-TYPE-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };

    // Injected input goes to the *focused* window, wherever that is. Refusing
    // here is the whole point of the check: without it, a window that lost focus
    // in the meantime turns this test into typing into someone else's
    // application.
    if !window.foreground {
        stop(&mut child);
        panic!(
            "the marker window lost focus before it could be typed into; injected input would \
             have gone to another application, so nothing was sent"
        );
    }

    let before = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} before typing");
        }
    };
    // The control: this same window, the same delay, nothing typed. Whatever it
    // measures is the window's idle behaviour, caret included, and the receipt
    // below is expressed against that measurement instead of against a pixel
    // count I would have to invent on a machine that has no console cell here.
    std::thread::sleep(PAINT_SETTLE);
    let control = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} with nothing typed");
        }
    };
    let noise = match before.changed_pixels(&control) {
        Some(moved) => moved,
        None => {
            stop(&mut child);
            panic!("the window changed geometry while nothing was being done to it");
        }
    };
    let typed = TYPED.repeat(TYPED_REPEATS);
    let delivery = match inject_text(&typed) {
        Ok(delivery) => delivery,
        Err(why) => {
            stop(&mut child);
            panic!("inject_text failed: {why}");
        }
    };
    std::thread::sleep(PAINT_SETTLE);
    let after = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} after typing");
        }
    };
    let _ = write_png("before-type.png", &before);
    let _ = write_png("control-type.png", &control);
    let _ = write_png("after-type.png", &after);
    stop(&mut child);

    let Delivery::Injected { events } = delivery else {
        stop(&mut child);
        panic!("inject_text must report an injected delivery");
    };
    assert_eq!(
        events,
        typed.chars().count() * 2,
        "{} characters are {} press/release pairs",
        typed.chars().count(),
        events
    );
    let moved = control
        .changed_pixels(&after)
        .expect("the same window photographed at the same size");
    let needed = noise
        .saturating_mul(SIGNAL_OVER_NOISE)
        .max(MIN_SIGNAL_PIXELS);
    assert!(
        moved > needed,
        "typing {typed:?} moved {moved} pixels, while this same console moved {noise} of its own \
         accord between two photographs with nothing typed in. A signal that does not clear its \
         own control by {SIGNAL_OVER_NOISE}x is not a receipt: either the characters never \
         reached this window, or the window never painted them. Note that a console with no \
         reader paints nothing at all, which is what the first version of this job measured.",
    );
    assert!(
        lit_pixels(&after) > lit_pixels(&control),
        "typing {typed:?} moved {moved} pixels but the frame gained no light \
         ({} -> {} lit pixels): that is a repaint without text, which is what a selection or a \
         scroll looks like and not what an echo looks like.",
        lit_pixels(&control),
        lit_pixels(&after)
    );
    note(&format!(
        "typed {} characters: {events} injected events, control noise {noise} px, moved {moved} px \
         (needed {needed}), light {} -> {} lit px",
        typed.chars().count(),
        lit_pixels(&control),
        lit_pixels(&after)
    ));
}

/// 2. An injected click puts the pointer on the point that was aimed at.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_an_injected_click_lands_where_it_was_aimed() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no pointer to move");
        return;
    }
    let marker = format!("OCIN-CLICK-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };
    // Aim at the middle of the client area rather than the window: the point has
    // to be inside the virtual desktop (it is) and on something the click is
    // allowed to touch.
    let aimed = ScreenPoint {
        x: window.rect.left + window.rect.width() / 2,
        y: window.rect.top + window.rect.height() / 2,
    };
    let result = inject_click(MouseButton::Left, aimed);
    let landed = match cursor_position() {
        Ok(point) => point,
        Err(why) => {
            stop(&mut child);
            panic!("GetCursorPos failed: {why}");
        }
    };
    stop(&mut child);
    let delivery = match result {
        Ok(delivery) => delivery,
        Err(why) => panic!("inject_click to ({},{}) failed: {why}", aimed.x, aimed.y),
    };

    // The scaling is lossy by construction: a 1024-wide desktop gives each pixel
    // 64 units, so the pointer can legitimately sit one pixel off the exact
    // centre of an odd-sized window. Anything larger than that is not rounding.
    let drift_x = (landed.x - aimed.x).abs();
    let drift_y = (landed.y - aimed.y).abs();
    assert!(
        drift_x <= 1 && drift_y <= 1,
        "aimed at ({},{}) and the pointer ended at ({},{}): drift {},{} exceeds one pixel, so \
         the absolute-coordinate scaling does not round-trip on this display ({:?})",
        aimed.x,
        aimed.y,
        landed.x,
        landed.y,
        drift_x,
        drift_y,
        delivery,
    );
    note(&format!(
        "aimed {},{} landed {},{} (drift {},{}) {:?}",
        aimed.x, aimed.y, landed.x, landed.y, drift_x, drift_y, delivery
    ));
}

/// 3. A posted click does not move the human's pointer.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_a_posted_click_leaves_the_cursor_alone() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no pointer to protect");
        return;
    }
    let marker = format!("OCIN-POST-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };
    let before = match cursor_position() {
        Ok(point) => point,
        Err(why) => {
            stop(&mut child);
            panic!("GetCursorPos failed: {why}");
        }
    };
    // Somewhere well away from where the pointer already is, so "it did not move"
    // cannot be satisfied by coincidence.
    let aimed = ScreenPoint {
        x: window.rect.left + 40,
        y: window.rect.top + 60,
    };
    let result = click_window(window.hwnd, MouseButton::Left, aimed);
    let after = match cursor_position() {
        Ok(point) => point,
        Err(why) => {
            stop(&mut child);
            panic!("GetCursorPos failed after the click: {why}");
        }
    };
    let _ = capture_stable(window.hwnd)
        .and_then(|capture| write_png("after-posted-click.png", &capture));
    stop(&mut child);

    let delivery = match result {
        Ok(delivery) => delivery,
        Err(why) => panic!("click_window failed: {why}"),
    };
    assert_eq!(
        before, after,
        "a posted click moved the pointer from {:?} to {:?}, which is exactly what this route \
         exists to avoid",
        before, after
    );
    let Delivery::Queued { messages } = delivery else {
        panic!("click_window must report a queued delivery, got {delivery:?}");
    };
    assert_eq!(messages, 3, "move, press, release");
    note(&format!(
        "posted click to {} at {},{} left the pointer at {:?} ({messages} messages queued)",
        window.hwnd, aimed.x, aimed.y, before
    ));
}

/// 4. The scan-code route puts the same characters on the same screen.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_the_scan_code_route_also_reaches_the_console() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no keyboard to press");
        return;
    }
    let marker = format!("OCIN-KEYS-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };
    if !window.foreground {
        stop(&mut child);
        panic!(
            "the marker window lost focus before it could be typed into; injected input would \
             have gone to another application, so nothing was sent"
        );
    }
    let before = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} before pressing keys");
        }
    };
    // The control, for the same reason as in the unicode job: the floor has to
    // be measured on this window, not assumed from a cell size I cannot see.
    std::thread::sleep(PAINT_SETTLE);
    let control = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} with nothing pressed");
        }
    };
    let noise = match before.changed_pixels(&control) {
        Some(moved) => moved,
        None => {
            stop(&mut child);
            panic!("the window changed geometry while nothing was being done to it");
        }
    };
    let typed = TYPED.repeat(TYPED_REPEATS);
    // The fallback path, not the unicode one: every character here is a key on
    // the installed layout, with Shift held where the layout says it must be.
    let delivery = match inject_text_as_keys(&typed) {
        Ok(delivery) => delivery,
        Err(why) => {
            stop(&mut child);
            panic!("inject_text_as_keys failed: {why}");
        }
    };
    std::thread::sleep(PAINT_SETTLE);
    let after = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} after pressing keys");
        }
    };
    let _ = write_png("before-keys.png", &before);
    let _ = write_png("control-keys.png", &control);
    let _ = write_png("after-keys.png", &after);
    stop(&mut child);

    let Delivery::Injected { events } = delivery else {
        panic!("inject_text_as_keys must report an injected delivery");
    };
    // Not an equality: how many events a string costs depends on the keyboard
    // the runner happens to have, because every shifted character carries two
    // more. What is layout-independent is the floor of one pair per character,
    // and that the count is even, since a lone press is a held key.
    assert!(
        events >= typed.chars().count() * 2,
        "{} events for {} characters is fewer than a press and a release each",
        events,
        typed.chars().count()
    );
    assert_eq!(
        events % 2,
        0,
        "{events} events means a key was left pressed: presses and releases are unbalanced"
    );
    let moved = control
        .changed_pixels(&after)
        .expect("the same window photographed at the same size");
    let needed = noise
        .saturating_mul(SIGNAL_OVER_NOISE)
        .max(MIN_SIGNAL_PIXELS);
    assert!(
        moved > needed,
        "the scan-code route sent {events} events and the frame moved {moved} pixels, while the \
         same untouched console moved {noise} between two photographs. This is the job that \
         separates the two keyboard contracts: the unicode route is a `WM_CHAR` to whoever holds \
         focus, while this one is a key on the keyboard, which is the only thing an application \
         that polls keys instead of reading characters can see. Events that arrive and paint \
         nothing are exactly what this message exists to catch.",
    );
    assert!(
        lit_pixels(&after) > lit_pixels(&control),
        "the scan-code route moved {moved} pixels but the frame gained no light ({} -> {} lit \
         pixels): a repaint without text is a selection or a scroll, not an echo.",
        lit_pixels(&control),
        lit_pixels(&after)
    );
    note(&format!(
        "scan-code route typed {} characters: {events} events, control noise {noise} px, moved \
         {moved} px (needed {needed}), light {} -> {} lit px",
        typed.chars().count(),
        lit_pixels(&control),
        lit_pixels(&after)
    ));
}
