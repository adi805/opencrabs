//! Tests for the input ABI, the arithmetic, and the event sequences.
//!
//! Cross-platform on purpose, like the other two pure test files: the failure
//! mode this suite defends against is a click that Windows *accepts* and then
//! delivers somewhere other than where it was aimed, and no Windows-side
//! assertion can see that. Everything here is arithmetic over bytes and
//! ordering, so it belongs in the Linux job that always runs, not in a runner
//! that may have no desktop.
//!
//! The syscall layer is covered in `desktop_windows_test`, which can only run on
//! a real Windows host.

use crate::desktop::{
    INPUT_BODY_BYTES, INPUT_KEYBOARD, INPUT_MOUSE, Input, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, KeyboardInput, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MouseButton, MouseInput, Rect, ScreenPoint, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, click_events, keyboard_input,
    mouse_input, normalize_axis,
};

fn desktop() -> Rect {
    Rect {
        left: 0,
        top: 0,
        right: 1024,
        bottom: 768,
    }
}

// ------------------------------------------------------------------ layout

#[cfg(target_pointer_width = "64")]
#[test]
fn the_input_abi_matches_win64() {
    // These numbers come from the Windows SDK and nothing in a Rust build
    // checks them: `#[repr(C)]` only promises C layout, so a field that stops
    // being a `LONG` keeps compiling and silently changes the size. `SendInput`
    // does compare the `cbSize` it is handed, which is why a wrong size fails
    // loudly rather than corrupting a gesture, but it then fails on *every*
    // call, so the number still has to be pinned somewhere that runs.
    assert_eq!(std::mem::size_of::<MouseInput>(), 32);
    assert_eq!(std::mem::size_of::<KeyboardInput>(), 24);
    assert_eq!(std::mem::size_of::<Input>(), 40);
    assert_eq!(std::mem::align_of::<Input>(), 8);
    assert_eq!(
        std::mem::size_of::<Input>(),
        8 + INPUT_BODY_BYTES,
        "the tag plus its alignment padding, then the body"
    );
}

#[cfg(not(target_pointer_width = "64"))]
#[test]
fn a_32bit_build_diverges_from_the_os_and_says_so() {
    // The body is pinned to 32 bytes on every target, so on 32-bit Windows this
    // is 36 where `winuser.h` says 28. The consequence is a rejected call (the
    // `cbSize` mismatch), never a misread event, and this test exists so the
    // x64 numbers above are never mistaken for a claim about this target.
    assert_eq!(std::mem::size_of::<Input>(), 4 + INPUT_BODY_BYTES);
    assert_ne!(std::mem::size_of::<Input>(), 28);
}

#[test]
fn the_body_sits_at_the_offset_the_tag_padding_leaves() {
    assert_eq!(std::mem::offset_of!(Input, kind), 0);
    assert_eq!(
        std::mem::offset_of!(Input, body),
        8,
        "Win64 pads the DWORD tag out to the union's 8-byte alignment"
    );
}

#[test]
fn every_member_of_the_union_starts_at_the_same_byte() {
    // The OS picks the member from the tag, so all of them must be addressed
    // from the same byte. A compiler-added offset on any member would make a
    // keyboard event be read out of the mouse struct's bytes.
    assert_eq!(
        std::mem::offset_of!(Input, body.mouse),
        std::mem::offset_of!(Input, body.keyboard)
    );
    assert_eq!(
        std::mem::offset_of!(Input, body.mouse),
        std::mem::offset_of!(Input, body.zeroed)
    );
}

#[test]
fn an_event_only_reads_back_the_member_its_tag_names() {
    // Reading the wrong member is not a compile error and not a panic: it
    // reinterprets bytes. The accessors make it `None`, which is the difference
    // between a caught bug and a plausible number.
    let key = keyboard_input(0x41, KEYEVENTF_UNICODE);
    assert_eq!(key.kind, INPUT_KEYBOARD);
    assert!(
        key.as_mouse().is_none(),
        "a keyboard event has no mouse member"
    );
    assert!(key.as_keyboard().is_some());
    let mouse = mouse_input(1, 2, MOUSEEVENTF_MOVE);
    assert_eq!(mouse.kind, INPUT_MOUSE);
    assert!(mouse.as_keyboard().is_none());
    assert_eq!(mouse.as_mouse().expect("tagged mouse").dx, 1);
    // A zeroed body is still a valid reading of either member: every field is a
    // plain integer with no invalid bit pattern.
    let zero = Input::zeroed(INPUT_MOUSE);
    assert_eq!(zero.as_mouse().expect("valid").flags, 0);
}

// ------------------------------------------------------------ coordinates

#[test]
fn absolute_coordinates_are_scaled_and_not_passed_through() {
    assert_eq!(normalize_axis(0, 0, 1024), Some(0));
    // 65535 / 1023 = 64.06, floored: one pixel is 64 units of pointer travel.
    assert_eq!(normalize_axis(1, 0, 1024), Some(64));
    assert_eq!(normalize_axis(100, 0, 1024), Some(6406));
}

#[test]
fn the_last_pixel_of_a_desktop_is_reachable() {
    // The divisor is `span - 1`, not `span`. With `span` the far corner maps to
    // 65534 and the rightmost column and bottom row become unclickable, which
    // reads as "that button does not respond" rather than as a maths error.
    assert_eq!(normalize_axis(1023, 0, 1024), Some(65535));
    assert_eq!(normalize_axis(767, 0, 768), Some(65535));
    // Two pixels: the endpoints are 0 and 65535, nothing in between.
    assert_eq!(normalize_axis(1, 0, 2), Some(65535));
}

#[test]
fn a_desktop_with_no_interior_is_refused_instead_of_divided() {
    for span in [0, 1, -5] {
        assert_eq!(
            normalize_axis(0, 0, span),
            None,
            "span {span} cannot be scaled"
        );
    }
}

#[test]
fn a_monitor_to_the_left_of_the_primary_keeps_its_coordinates() {
    // The virtual desktop starts at the *leftmost* monitor, which can be a
    // negative origin, and scaling must subtract that origin before dividing.
    // Skipping the subtraction works on a single-monitor laptop and puts every
    // click 1920 pixels off on the setup that motivated it.
    assert_eq!(normalize_axis(-1920, -1920, 2944), Some(0));
    assert_eq!(normalize_axis(1023, -1920, 2944), Some(65535));
    assert_eq!(normalize_axis(-1921, -1920, 2944), None);
}

#[test]
fn a_point_outside_the_desktop_is_refused_and_not_clamped() {
    assert_eq!(normalize_axis(1024, 0, 1024), None);
    assert_eq!(normalize_axis(-1, 0, 1024), None);
    assert!(
        click_events(
            &desktop(),
            MouseButton::Left,
            ScreenPoint { x: 4000, y: 100 }
        )
        .is_none(),
        "clamping sends the click to the screen edge, where some other \
         application's control lives"
    );
}

// ------------------------------------------------------------------ clicks

#[test]
fn an_injected_click_is_hover_then_press_then_release() {
    let events = click_events(
        &desktop(),
        MouseButton::Left,
        ScreenPoint { x: 100, y: 200 },
    )
    .expect("the point is inside the desktop");
    assert_eq!(events.len(), 3, "move, down, up");
    let absolute = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
    assert_eq!(events[0].as_mouse().expect("mouse").flags, absolute);
    assert_eq!(
        events[1].as_mouse().expect("mouse").flags,
        absolute | MOUSEEVENTF_LEFTDOWN
    );
    assert_eq!(
        events[2].as_mouse().expect("mouse").flags,
        absolute | MOUSEEVENTF_LEFTUP
    );
}

#[test]
fn every_event_of_a_click_carries_the_same_position() {
    // The press and release are absolute too. Omitting the coordinates there
    // makes the button come up wherever the pointer ended up after the previous
    // event, which turns a click into a drag onto a different control.
    let events = click_events(
        &desktop(),
        MouseButton::Right,
        ScreenPoint { x: 100, y: 200 },
    )
    .expect("inside");
    let first = events[0].as_mouse().expect("mouse");
    for event in &events {
        let mouse = event
            .as_mouse()
            .expect("every event in a click is a mouse event");
        assert_eq!((mouse.dx, mouse.dy), (first.dx, first.dy));
        assert_eq!(event.kind, INPUT_MOUSE);
    }
    assert_eq!((first.dx, first.dy), (6406, 17088));
}

#[test]
fn each_button_pairs_its_press_with_its_release() {
    // Every button has three separate encodings: injected flags, posted
    // messages, and the `MK_*` state bit. A pairing that is wrong in any one of
    // them produces a click that never comes up, so all three are checked
    // against one table, which is the only way to notice they disagree.
    let rows: [(MouseButton, u32, u32, u32, u32, usize); 3] = [
        (
            MouseButton::Left,
            MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP,
            WM_LBUTTONDOWN,
            WM_LBUTTONUP,
            0x0001,
        ),
        (
            MouseButton::Right,
            MOUSEEVENTF_RIGHTDOWN,
            MOUSEEVENTF_RIGHTUP,
            WM_RBUTTONDOWN,
            WM_RBUTTONUP,
            0x0002,
        ),
        (
            MouseButton::Middle,
            MOUSEEVENTF_MIDDLEDOWN,
            MOUSEEVENTF_MIDDLEUP,
            WM_MBUTTONDOWN,
            WM_MBUTTONUP,
            0x0010,
        ),
    ];
    for (button, down, up, down_message, up_message, mk) in rows {
        assert_eq!(button.down_flag(), down, "{button:?} press flag");
        assert_eq!(button.up_flag(), up, "{button:?} release flag");
        assert_eq!(button.down_message(), down_message, "{button:?} press msg");
        assert_eq!(button.up_message(), up_message, "{button:?} release msg");
        assert_eq!(button.mk_flag(), mk, "{button:?} state bit");
        assert_eq!(
            down & up,
            0,
            "{button:?} press and release share a bit, so one event cannot be told \
             from the other"
        );
    }
    // Distinct across buttons too: two buttons sharing a release bit would let a
    // right-click release be delivered as a middle-click release.
    let releases: std::collections::HashSet<u32> = rows.iter().map(|row| row.2).collect();
    assert_eq!(releases.len(), 3, "each button needs its own release bit");
    let messages: std::collections::HashSet<u32> =
        rows.iter().flat_map(|row| [row.3, row.4]).collect();
    assert_eq!(messages.len(), 6, "each button needs its own message pair");
}

#[test]
fn the_mouse_flag_bits_do_not_overlap() {
    // `flags` is a bit set, so a duplicated constant silently merges two
    // meanings: "move and press" would become indistinguishable from "scroll",
    // and neither side of the call could tell that it happened.
    let bits = [
        MOUSEEVENTF_MOVE,
        MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP,
        MOUSEEVENTF_WHEEL,
        MOUSEEVENTF_VIRTUALDESK,
        MOUSEEVENTF_ABSOLUTE,
    ];
    for (index, first) in bits.iter().enumerate() {
        for second in &bits[index + 1..] {
            assert_eq!(
                first & second,
                0,
                "mouse flags {first:#x} and {second:#x} share a bit"
            );
        }
    }
}

#[test]
fn the_keyboard_flag_bits_do_not_overlap() {
    // The key bits are a different namespace from the mouse bits and reuse some
    // of the same values (`KEYEVENTF_EXTENDEDKEY` and `MOUSEEVENTF_MOVE` are
    // both 0x0001), which is exactly why the two sets are checked separately.
    let bits = [
        KEYEVENTF_EXTENDEDKEY,
        KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE,
        KEYEVENTF_SCANCODE,
    ];
    for (index, first) in bits.iter().enumerate() {
        for second in &bits[index + 1..] {
            assert_eq!(
                first & second,
                0,
                "keyboard flags {first:#x} and {second:#x} share a bit"
            );
        }
    }
}
