//! Cross-platform shapes and selection rules for [`super`].
//!
//! The rules live here, away from the syscalls, for one reason: they decide
//! what an agent gets to see and therefore what it can act on, and they are
//! testable without a desktop. The backend files stay thin marshalling layers.

use std::fmt;

/// Window bounds in physical screen pixels, exactly as Win32 reports them:
/// the origin is the top-left of the primary monitor and coordinates can be
/// negative, which is normal for a monitor placed to the left or above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    pub fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }

    /// The signed, saturating product of the spans. This is arithmetic, not a
    /// validity test: a rect with both edges swapped (right < left *and*
    /// bottom < top) multiplies out to a positive number, so no caller should
    /// treat `area() > 0` as "this window is real". Use [`Rect::has_positive_span`].
    pub fn area(&self) -> i64 {
        i64::from(self.width()).saturating_mul(i64::from(self.height()))
    }

    /// Whether the rect describes a region that could actually be painted.
    ///
    /// Both spans must be positive independently. The single test that reads
    /// most naturally, `area() > 0`, is the one that lies: inverting both edges
    /// keeps the product positive, and a nonsense rect therefore looks
    /// indistinguishable from an 800x600 window. Negative *origins* are fine
    /// and expected (secondary monitors up and to the left), which is why the
    /// test is on the spans and not on the coordinates.
    pub fn has_positive_span(&self) -> bool {
        self.width() > 0 && self.height() > 0
    }
}

/// One top-level window as the OS sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// The Win32 `HWND`, held as a pointer-sized integer so this type compiles
    /// on every target. It is never `0`: `EnumWindows` only hands out real
    /// handles, and `0` is the value `GetForegroundWindow` uses for "nothing",
    /// so the two are not confusable.
    pub hwnd: isize,
    /// Owning process id, `0` when the query failed. Kept as a real field
    /// rather than an `Option` because the Windows API reports failure that
    /// way already, and callers filter on it.
    pub pid: u32,
    pub title: String,
    /// Window class name (the `RegisterClass` atom, not the title). Used by
    /// [`is_shell_backdrop`] and, more importantly, as the stable identifier
    /// for automated driving: titles get localised and renamed.
    pub class: String,
    pub rect: Rect,
    /// True for the one window currently holding keyboard focus.
    pub foreground: bool,
}

impl WindowInfo {
    /// A window whose rect cannot describe a painted region. Reported rather
    /// than silently dropped by the caller, so a backend that forgot the filter
    /// is visible in a test instead of invisible in a click that lands nowhere.
    pub fn is_degenerate(&self) -> bool {
        !self.rect.has_positive_span()
    }
}

/// Hard ceiling for one snapshot. Deliberate, not a tuning knob: a desktop
/// with 2,000 windows (a browser with many tabs plus a few Electron apps is
/// enough) would otherwise dump the whole window list into the model's context
/// every turn, and callers cannot tell a real list from a truncated one unless
/// the type says so. See [`WindowList::truncated`].
pub const MAX_WINDOWS: usize = 256;

/// Window classes that paint the desktop itself rather than being an app an
/// agent could usefully drive: the wallpaper layer, its sibling windows, and
/// the taskbar. They are visible, they have area, and they are noise: offered
/// as click targets, they invite an agent to "click the desktop" and call it
/// progress. Windows keeps the class names stable across localisations even
/// when the titles are localised, so the filter survives a non-English host.
const SHELL_BACKDROPS: [&str; 3] = ["Progman", "WorkerW", "Shell_TrayWnd"];

/// Whether a class name belongs to the desktop furniture rather than an app.
pub fn is_shell_backdrop(class: &str) -> bool {
    SHELL_BACKDROPS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(class))
}

/// The candidate rule: visible, a span that could be painted, not furniture.
///
/// Deliberately *not* `!title.is_empty()`: untitled windows are often exactly
/// the ones worth driving (a dialog that has not set its caption yet, a game,
/// a Chrome render surface), and dropping them would hide interactive targets
/// behind a cosmetic property.
pub fn keep_candidate(visible: bool, rect: &Rect, class: &str) -> bool {
    visible && rect.has_positive_span() && !is_shell_backdrop(class)
}

/// A window snapshot plus whether it was cut short.
///
/// `truncated` is part of the type rather than a log line because a truncated
/// list that looks complete is the failure mode that matters: an agent that
/// cannot find its target window concludes "the app is not running" and starts
/// doing something worse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowList {
    pub windows: Vec<WindowInfo>,
    pub truncated: bool,
}

impl WindowList {
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// The window holding keyboard focus, if it survived filtering.
    pub fn foreground(&self) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.foreground)
    }
}

impl fmt::Display for WindowList {
    /// One line per window, in the order the OS enumerated them (z-order,
    /// front first). Bounded by [`MAX_WINDOWS`] by construction, so this can
    /// never dump an unbounded string into a prompt or a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for w in &self.windows {
            writeln!(
                f,
                "hwnd={} pid={} {}x{} {:+},{} [{}] {}",
                w.hwnd,
                w.pid,
                w.rect.width(),
                w.rect.height(),
                w.rect.left,
                w.rect.top,
                w.class,
                w.title
            )?;
        }
        if self.truncated {
            writeln!(f, "(truncated at {MAX_WINDOWS} windows)")?;
        }
        Ok(())
    }
}

/// A channel value at or below this counts as "no ink".
///
/// Not zero, deliberately. A captured window has antialiased text, so the edge
/// of a glyph is not full-intensity, and on some display drivers the background
/// is not exactly `#000000`; a small floor keeps "blank" about the absence of
/// content rather than about the exactness of a black pixel. 16 is well under
/// the value any visible glyph edge carries (a dim antialiased pixel still lands
/// around 100), so this ignores near-black noise without hiding real text.
pub const INK_CHANNEL_FLOOR: u8 = 16;

/// The smallest fraction of inked pixels that counts as a real capture.
///
/// This is the guard the capture path exists for: `PrintWindow` returns TRUE for
/// windows it failed to paint, so a caller that checks only the return value
/// reports a solid black rectangle as evidence that it saw the window. Measured
/// on a real runner, the failure mode is a frame with *exactly zero* inked
/// pixels, so the threshold only has to sit above stray driver noise.
///
/// The value is pinned to a glyph rather than picked: 0.0001 of a 1024x768 frame
/// is 79 pixels, which is under one 8x16 character (128 pixels). So a frame
/// carrying even a single character of text is never called blank, while a black
/// frame at zero is. `one_glyph_still_clears_the_blank_threshold` in
/// `tests::desktop_capture_test` asserts that arithmetic, so moving this
/// constant is a decision with a test that reacts to it.
pub const MIN_INK_RATIO: f64 = 0.0001;

/// Fraction of pixels carrying any ink, in `0.0..=1.0`.
///
/// Alpha is ignored on purpose. The DIB that `GetDIBits` fills for a `BI_RGB`
/// 32-bpp surface leaves the fourth byte at zero, so a test that also required
/// alpha to be set would call every honest capture blank.
pub fn ink_ratio(rgba: &[u8]) -> f64 {
    let pixels = rgba.len() / 4;
    if pixels == 0 {
        return 0.0;
    }
    let inked = rgba
        .chunks_exact(4)
        .filter(|p| {
            p[0] > INK_CHANNEL_FLOOR || p[1] > INK_CHANNEL_FLOOR || p[2] > INK_CHANNEL_FLOOR
        })
        .count();
    inked as f64 / pixels as f64
}

/// Rewrite a `BGRA` buffer in place as `RGBA` with opaque alpha.
///
/// Win32 hands out `BGRA` for a `BI_RGB` 32-bpp DIB and `image` wants `RGBA`.
/// The alpha byte is set rather than swapped because the DIB leaves it at zero,
/// and a fully transparent PNG is indistinguishable from a blank one to
/// anything that composites it.
pub fn bgra_to_rgba(buffer: &mut [u8]) {
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
}

/// One window, photographed, with the blankness verdict computed once.
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA, `width * height * 4` bytes, top row first.
    pub rgba: Vec<u8>,
    /// [`ink_ratio`] of `rgba`, computed at construction so no caller can forget
    /// to check it and no two callers can disagree about the same frame.
    pub ink_ratio: f64,
}

impl Capture {
    /// Wrap a pixel buffer, or `None` when its length contradicts the size.
    ///
    /// A mismatched buffer is not a capture with a cosmetic problem: every
    /// consumer indexes it as a grid, so accepting it moves the failure into
    /// whichever code reads pixel `(x, y)` later, far from the cause.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if rgba.len() != expected {
            return None;
        }
        let ink_ratio = ink_ratio(&rgba);
        Some(Self {
            width,
            height,
            rgba,
            ink_ratio,
        })
    }

    /// Whether this capture carries no visible content.
    pub fn is_blank(&self) -> bool {
        self.ink_ratio < MIN_INK_RATIO
    }

    /// Encode as PNG.
    ///
    /// The `png` feature is already enabled for the WhatsApp pairing QR, so this
    /// adds no dependency and no feature flag.
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let img = image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| {
                format!(
                    "buffer of {} bytes does not describe {}x{} RGBA",
                    self.rgba.len(),
                    self.width,
                    self.height
                )
            })?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| format!("PNG encode failed: {e}"))?;
        Ok(out.into_inner())
    }
}
