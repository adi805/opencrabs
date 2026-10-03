//! #1816: the clipboard hint fires only for an image-only clipboard, the
//! exact shape of a screenshot copied to the clipboard. Finder and browser
//! copies carry a text flavor next to the image, so they stay quiet.

use crate::tui::app::clipboard_route::{
    clipboard_info_fingerprint, clipboard_info_has_image, clipboard_info_has_text,
    clipboard_info_is_image_only,
};

/// Real `clipboard info` output for a macOS screenshot copied to the
/// clipboard: nine image classes, zero text classes.
const SCREENSHOT_INFO: &str = "«class PNGf», 22396, «class AVIF», 3219, «class 8BPS», 103000, \
GIF picture, 8071, «class jp2 », 16380, JPEG picture, 12842, TIFF picture, 1497170, \
«class BMP », 1493722, «class TPIC», 34885";

#[test]
fn a_screenshot_clipboard_is_image_only() {
    assert!(clipboard_info_is_image_only(SCREENSHOT_INFO));
}

#[test]
fn an_image_with_a_text_flavor_is_not_image_only() {
    // Finder file copy: file URL and text next to the icon image.
    let info = "«class PNGf», 22396, string, 42, «class utf8», 42, «class furl», 120";
    assert!(clipboard_info_has_image(info));
    assert!(clipboard_info_has_text(info));
    assert!(!clipboard_info_is_image_only(info));
}

#[test]
fn text_only_clipboards_never_hint() {
    assert!(clipboard_info_has_text(
        "string, 5, «class utf8», 5, «class ut16», 5"
    ));
    assert!(!clipboard_info_has_image("string, 5, «class utf8», 5"));
    assert!(!clipboard_info_is_image_only("string, 5, «class utf8», 5"));
}

#[test]
fn an_empty_clipboard_never_hints() {
    assert!(!clipboard_info_is_image_only(""));
    assert!(!clipboard_info_is_image_only("   \n  "));
}

#[test]
fn the_fingerprint_ignores_whitespace_but_not_content() {
    let a = clipboard_info_fingerprint("«class PNGf», 22396, TIFF picture, 1497170");
    let b = clipboard_info_fingerprint("«class PNGf»,  22396,  TIFF  picture,  1497170");
    let c = clipboard_info_fingerprint("«class PNGf», 99, TIFF picture, 1497170");
    assert_eq!(a, b);
    assert_ne!(a, c);
}
