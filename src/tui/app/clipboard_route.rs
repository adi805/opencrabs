//! What Ctrl+V / Cmd+V should read from the OS clipboard (#1811).
//!
//! A screenshot copied to the clipboard carries image classes and no text
//! class at all, so a text-only read comes back empty. Text still goes first:
//! a Finder file copy holds both the filename and an icon image, and the
//! filename is what the user meant to paste.

/// Stable fingerprint of a clipboard's `clipboard info` output, used to
/// tell "same image still sitting there" from "a new screenshot". Collapses
/// whitespace so class/size pairs compare equal across osascript runs.
pub(crate) fn clipboard_info_fingerprint(info: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    info.split_whitespace()
        .collect::<Vec<_>>()
        .hash(&mut hasher);
    hasher.finish()
}

/// AppleScript `clipboard info` class fragments that mean image bytes are on
/// the clipboard. "picture" covers the TIFF/JPEG/GIF picture classes; the
/// rest are raw `«class ...»` codes.
const IMAGE_CLASS_MARKERS: &[&str] = &[
    "PNGf", "picture", "TIFF", "JPEG", "GIF", "BMP", "jp2", "AVIF", "8BPS", "TPIC", "PICT",
];

/// Class fragments that carry text. Any of these means the user can paste
/// text right now, so an image hint would be noise: Finder and browser
/// copies carry a text flavor next to the image.
const TEXT_CLASS_MARKERS: &[&str] = &["string", "utf8", "ut16", "utxt", "Unicode text"];

/// True when the clipboard holds image classes and no text class, the exact
/// shape of a screenshot copied to the clipboard (#1816).
pub(crate) fn clipboard_info_is_image_only(info: &str) -> bool {
    clipboard_info_has_image(info) && !clipboard_info_has_text(info)
}

/// True when any known image class appears in `clipboard info` output.
pub(crate) fn clipboard_info_has_image(info: &str) -> bool {
    IMAGE_CLASS_MARKERS.iter().any(|m| info.contains(m))
}

/// True when any known text class appears in `clipboard info` output.
pub(crate) fn clipboard_info_has_text(info: &str) -> bool {
    TEXT_CLASS_MARKERS.iter().any(|m| info.contains(m))
}

/// Raw `clipboard info` output, macOS only: `None` everywhere else so the
/// hint stays silent off macOS. Read-only, it never touches the clipboard.
pub(crate) fn read_clipboard_info() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        match Command::new("osascript")
            .arg("-e")
            .arg("clipboard info")
            .output()
        {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8(out.stdout).ok()?;
                (!text.is_empty()).then_some(text)
            }
            Ok(out) => {
                tracing::debug!(
                    stderr = %String::from_utf8_lossy(&out.stderr),
                    "clipboard info probe failed"
                );
                None
            }
            Err(e) => {
                tracing::debug!("clipboard info probe could not run: {e}");
                None
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

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
