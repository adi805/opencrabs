//! Tests for the capture shapes: ink measurement, pixel format, and the blank
//! verdict.
//!
//! Cross-platform on purpose, like `desktop_model_test`. The part of capture
//! that decides whether a frame is evidence is pure arithmetic, and arithmetic
//! is exactly what should be tested on every host instead of only on the one
//! runner that happens to have a desktop.
//!
//! The Windows syscall path is covered separately in `desktop_windows_test`,
//! which can only run on a real Windows host.

use crate::desktop::{Capture, INK_CHANNEL_FLOOR, MIN_INK_RATIO, bgra_to_rgba, ink_ratio};

/// A frame of `width` x `height` filled with one colour, as RGBA.
fn frame(width: u32, height: u32, r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for _ in 0..(width as usize * height as usize) {
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

#[test]
fn an_all_black_frame_measures_zero_ink() {
    // The observed failure mode on a real runner is a frame with exactly zero
    // inked pixels, so zero has to read as zero before anything else about this
    // measurement can be trusted.
    assert_eq!(ink_ratio(&frame(64, 48, 0, 0, 0, 0)), 0.0);
    // And the channel floor must not leak: one level above pure black is still
    // below it, or "blank" would be decided by driver noise.
    assert_eq!(ink_ratio(&frame(64, 48, 1, 1, 1, 255)), 0.0);
    assert_eq!(
        ink_ratio(&frame(
            64,
            48,
            INK_CHANNEL_FLOOR,
            INK_CHANNEL_FLOOR,
            INK_CHANNEL_FLOOR,
            255
        )),
        0.0,
        "the floor is inclusive: a channel at exactly the floor is not ink"
    );
}

#[test]
fn ink_is_counted_per_pixel_not_per_channel() {
    // A single lit channel makes the whole pixel count once. Counting channels
    // would inflate a red-only frame to 1/3 of its pixels and make the ratio
    // depend on the colour of the text.
    let mut one_red = frame(10, 10, 0, 0, 0, 255);
    one_red[0] = 255;
    assert_eq!(ink_ratio(&one_red), 1.0 / 100.0);

    // A fully lit frame is 1.0, not 3.0.
    assert_eq!(ink_ratio(&frame(10, 10, 255, 255, 255, 255)), 1.0);
}

#[test]
fn ink_ignores_alpha_because_a_dib_leaves_it_zero() {
    // This is the trap the whole check would fall into: `GetDIBits` fills a
    // BI_RGB 32-bpp surface with the fourth byte at zero, so requiring alpha to
    // be set would call every honest capture blank.
    let mut dib_shaped = frame(8, 8, 255, 255, 255, 0);
    assert_eq!(ink_ratio(&dib_shaped), 1.0);

    // The mirror case: alpha set but no colour is still no content.
    let mut opaque_black = frame(8, 8, 0, 0, 0, 255);
    assert_eq!(ink_ratio(&opaque_black), 0.0);

    // And the alpha byte is not read as a colour channel, which would make an
    // otherwise black frame measure as fully inked.
    dib_shaped[3] = 255;
    opaque_black[3] = 255;
    assert_eq!(ink_ratio(&dib_shaped), 1.0);
    assert_eq!(ink_ratio(&opaque_black), 0.0);
}

#[test]
fn one_glyph_still_clears_the_blank_threshold() {
    // The blank threshold has to be low enough that the smallest thing we would
    // ever want to see still counts as content. One 8x16 character on the frame
    // size the probe measured is the concrete worst case, so the constant is
    // checked against that rather than against a feeling about percentages.
    let probe_frame_pixels = 1024.0 * 768.0;
    let one_glyph = 8.0 * 16.0;
    assert!(
        MIN_INK_RATIO * probe_frame_pixels < one_glyph,
        "{MIN_INK_RATIO} of a 1024x768 frame is {} pixels, which is not below one 8x16 glyph ({one_glyph})",
        MIN_INK_RATIO * probe_frame_pixels
    );
    assert!(MIN_INK_RATIO > 0.0, "a zero threshold cannot fail");
}

#[test]
fn a_capture_with_one_character_is_not_blank_and_a_black_one_is() {
    // The same claim end to end, through the type a caller actually holds: a
    // frame with a single glyph's worth of lit pixels survives the blank check,
    // and an empty frame does not.
    let (w, h) = (1024u32, 768u32);
    let mut with_text = frame(w, h, 0, 0, 0, 255);
    // Exactly one 8x16 glyph's worth of lit pixels, which is the smallest amount
    // of content this frame can carry and still be called content.
    for pixel in with_text.chunks_exact_mut(4).take(8 * 16) {
        pixel[0] = 200;
        pixel[1] = 200;
        pixel[2] = 200;
    }
    let text = Capture::from_rgba(w, h, with_text).expect("sized buffer");
    assert!(
        !text.is_blank(),
        "one character of text measured {:.6}, which the blank check rejected",
        text.ink_ratio
    );

    let black = Capture::from_rgba(w, h, frame(w, h, 0, 0, 0, 255)).expect("sized buffer");
    assert!(black.is_blank(), "an all-black frame was accepted");
    assert_eq!(black.ink_ratio, 0.0);
}

#[test]
fn a_buffer_that_contradicts_its_dimensions_is_refused() {
    // Every consumer indexes the buffer as a grid, so accepting a short buffer
    // moves the failure into whichever code reads pixel (x, y) later, far from
    // the cause. `None` is the honest answer.
    assert!(Capture::from_rgba(4, 4, vec![0u8; 4 * 4 * 4]).is_some());
    assert!(Capture::from_rgba(4, 4, vec![0u8; 4 * 4 * 4 - 1]).is_none());
    assert!(Capture::from_rgba(4, 4, vec![0u8; 4 * 4 * 4 + 4]).is_none());
    assert!(Capture::from_rgba(4, 4, Vec::new()).is_none());
    // A size whose product overflows is not a capture either, and must not panic
    // in a release build where the multiply wraps.
    assert!(Capture::from_rgba(u32::MAX, u32::MAX, vec![0u8; 16]).is_none());
}

#[test]
fn bgra_is_reordered_and_made_opaque() {
    // Win32 hands out BGRA; `image` wants RGBA. A swap without the alpha write
    // produces a transparent PNG, which composites to nothing and is
    // indistinguishable from a blank capture to whoever looks at it.
    let mut buffer = vec![
        0x11, 0x22, 0x33, 0x00, // one pixel: B=0x11 G=0x22 R=0x33 A=0x00
        0xFF, 0x00, 0x00, 0x00, // B=0xFF G=0x00 R=0x00
    ];
    bgra_to_rgba(&mut buffer);
    assert_eq!(buffer, vec![0x33, 0x22, 0x11, 0xFF, 0x00, 0x00, 0xFF, 0xFF]);
}

#[test]
fn bgra_conversion_survives_a_trailing_partial_pixel() {
    // `chunks_exact_mut` ignores a remainder instead of panicking. A capture
    // buffer is always a whole number of pixels by construction, but this
    // function is public and the behaviour should be stated, not inferred.
    let mut buffer = vec![0x11, 0x22, 0x33, 0x00, 0xAA];
    bgra_to_rgba(&mut buffer);
    assert_eq!(buffer, vec![0x33, 0x22, 0x11, 0xFF, 0xAA]);
}

#[test]
fn a_capture_round_trips_through_png() {
    // The PNG feature is already enabled for the WhatsApp pairing QR, so this
    // path adds no dependency. Decoding it back is the only way to know the
    // bytes are a real image rather than a header with a truncated payload.
    let capture = Capture::from_rgba(
        3,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, //
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255,
        ],
    )
    .expect("sized buffer");

    let png = capture.to_png().expect("encode");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");

    let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .expect("decode what we just encoded")
        .to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (3, 2));
    assert_eq!(decoded.into_raw(), capture.rgba);
}

#[test]
fn a_blank_capture_still_encodes() {
    // The capture path writes the PNG *before* it fails the blank check, so the
    // evidence survives the failure and a CI job's `if: always()` upload still
    // carries it. That only works if a blank frame encodes.
    let black = Capture::from_rgba(4, 4, frame(4, 4, 0, 0, 0, 255)).expect("sized buffer");
    assert!(black.is_blank());
    let png = black.to_png().expect("a blank frame must still encode");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
}

#[cfg(not(windows))]
#[test]
fn the_linux_stub_reports_the_missing_backend_instead_of_a_blank_frame() {
    // Fail-closed, and specifically not "return an empty capture": a stub that
    // produced a black `Capture` would be indistinguishable from a window that
    // genuinely painted nothing, and the caller would save an empty PNG and call
    // it a screenshot. This test runs on the Linux job, which is the only place
    // the stub is compiled.
    let error = crate::desktop::capture_window(1234).expect_err("no backend on this platform");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);

    let error = crate::desktop::capture_window_to_png(1234, std::path::Path::new("/dev/null"))
        .expect_err("no backend on this platform");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}
