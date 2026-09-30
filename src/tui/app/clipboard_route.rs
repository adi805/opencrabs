//! What Ctrl+V / Cmd+V should read from the OS clipboard (#1811).
//!
//! A screenshot copied to the clipboard carries image classes and no text
//! class at all, so a text-only read comes back empty. Text still goes first:
//! a Finder file copy holds both the filename and an icon image, and the
//! filename is what the user meant to paste.

/// Where a clipboard paste goes, decided from the clipboard's text alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardRoute<'a> {
    /// Usable text: a file path to attach or prose to insert.
    Text(&'a str),
    /// No usable text: try the clipboard's raw image bytes instead.
    Image,
}

/// Route on the clipboard text. `None` (every text backend failed) and
/// whitespace-only text both mean the clipboard holds no text to paste.
pub(crate) fn route_for_clipboard_text(text: Option<&str>) -> ClipboardRoute<'_> {
    match text {
        Some(t) if !t.trim().is_empty() => ClipboardRoute::Text(t),
        _ => ClipboardRoute::Image,
    }
}
