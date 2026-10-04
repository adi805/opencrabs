//! #1811: Ctrl+V must fall back to the clipboard image when the clipboard
//! carries no text (a screenshot copied to the clipboard holds image classes
//! only), and must keep text first when there is any.

use crate::tui::app::clipboard_route::{ClipboardRoute, route_for_clipboard_text};

#[test]
fn unreadable_clipboard_text_routes_to_the_image_read() {
    assert_eq!(route_for_clipboard_text(None), ClipboardRoute::Image);
}

#[test]
fn empty_clipboard_text_routes_to_the_image_read() {
    assert_eq!(route_for_clipboard_text(Some("")), ClipboardRoute::Image);
}

#[test]
fn whitespace_only_clipboard_text_routes_to_the_image_read() {
    assert_eq!(
        route_for_clipboard_text(Some("  \n\t ")),
        ClipboardRoute::Image
    );
}

#[test]
fn real_text_stays_on_the_text_route_untrimmed() {
    assert_eq!(
        route_for_clipboard_text(Some(" hello\n")),
        ClipboardRoute::Text(" hello\n")
    );
}

#[test]
fn file_path_text_wins_over_an_image_on_the_same_clipboard() {
    // A Finder copy carries the filename and an icon image; the filename is
    // the paste the user meant.
    assert_eq!(
        route_for_clipboard_text(Some("/tmp/report.pdf")),
        ClipboardRoute::Text("/tmp/report.pdf")
    );
}
