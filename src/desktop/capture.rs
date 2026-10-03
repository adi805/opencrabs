//! Photographing one window, on Windows.
//!
//! This is the read half of desktop control that enumeration cannot cover: a
//! window list says a title and a rectangle, and the only way to know what is
//! *inside* that rectangle is to ask the window to paint itself. The agent then
//! has pixels to reason about instead of a guess about which button is where.
//!
//! Two traps this file exists to close, both of which report success:
//!
//! 1. `PrintWindow` returns TRUE for windows it did not manage to paint. A
//!    caller that checks only the return value gets a black rectangle and calls
//!    it a screenshot. That is why [`capture_window_to_png`] measures ink and
//!    fails on a blank frame, and why the measurement is deterministic
//!    arithmetic in `super::model` rather than a model's opinion.
//! 2. `GetDIBits` hands back `BGRA` for a `BI_RGB` 32-bpp surface, with the
//!    fourth byte left at zero. Writing that straight out as PNG produces a
//!    fully transparent image that looks identical to a blank one to anything
//!    that composites it, so the alpha byte is set on the way through.
//!
//! Deliberately absent: anything that draws on the screen (no `BitBlt` of the
//! whole display). Photographing the desktop would capture whatever the user
//! has open, including windows this agent was never granted, and the window
//! handle is the unit the caller actually asked about.

use super::model::{Capture, Rect, bgra_to_rgba};
use super::win32::{
    BI_RGB, BitmapInfo, BitmapInfoHeader, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, GetWindowRect, PW_RENDERFULLCONTENT,
    PrintWindow, ReleaseDC, RgbQuad, SelectObject, WinRect,
};
use std::io;
use std::path::Path;

/// `SelectObject` returns this sentinel (`HGDI_ERROR`, a `(HGDIOBJ)-1`) when the
/// object it was given does not belong to the DC's format.
const HGDI_ERROR: isize = -1;

/// The GDI handles one capture borrows, released in reverse order on drop.
///
/// A guard rather than a sequence of calls with early returns, because every
/// failure path after the first allocation would otherwise leak: `GetDC` and
/// `CreateCompatibleBitmap` leak *GDI objects*, a process has a hard quota of
/// 10,000 of them, and exhausting it breaks drawing for the whole process. A
/// leak here would surface minutes later as unrelated windows failing to paint.
struct DibSurface {
    screen_dc: isize,
    mem_dc: isize,
    bitmap: isize,
    /// The stock object `CreateCompatibleDC` selects into a fresh DC. It must go
    /// back before the DC is deleted, or the DC is destroyed while still holding
    /// our bitmap.
    previous: isize,
}

impl DibSurface {
    /// A memory DC holding an empty `width` x `height` bitmap.
    ///
    /// The fields start at zero so a failure part-way through leaves the
    /// already-allocated handles in place for `Drop` to release: returning early
    /// from a half-built guard is safe *because* nothing is allocated before the
    /// corresponding field is written.
    fn new(width: i32, height: i32) -> io::Result<Self> {
        let mut surface = Self {
            screen_dc: 0,
            mem_dc: 0,
            bitmap: 0,
            previous: 0,
        };
        // `GetDC` reports failure by returning NULL and does not reliably set
        // the last error, so the message is written rather than read.
        surface.screen_dc = unsafe { GetDC(0) };
        if surface.screen_dc == 0 {
            return Err(io::Error::other("GetDC(NULL) returned no screen context"));
        }
        surface.mem_dc = unsafe { CreateCompatibleDC(surface.screen_dc) };
        if surface.mem_dc == 0 {
            return Err(io::Error::other("CreateCompatibleDC returned no context"));
        }
        // The screen DC is the template on purpose: it carries the display's
        // colour depth, so the bitmap matches what the window will paint.
        surface.bitmap = unsafe { CreateCompatibleBitmap(surface.screen_dc, width, height) };
        if surface.bitmap == 0 {
            return Err(io::Error::other(format!(
                "CreateCompatibleBitmap refused {width}x{height}"
            )));
        }
        surface.previous = unsafe { SelectObject(surface.mem_dc, surface.bitmap) };
        if surface.previous == 0 || surface.previous == HGDI_ERROR {
            return Err(io::Error::other("SelectObject refused the capture bitmap"));
        }
        Ok(surface)
    }

    /// Put the DC's previous object back, so the capture bitmap is no longer
    /// selected into it.
    ///
    /// `GetDIBits` requires this: its documentation says the bitmap must not be
    /// selected into a device context when it is read, and reading one that is
    /// still selected can fail or return unreliable pixels. Drop restores it
    /// too, so this zeroes `previous` to keep that from running twice.
    fn deselect(&mut self) {
        if self.previous != 0 && self.previous != HGDI_ERROR {
            // Safety: `previous` is what `SelectObject` returned for `mem_dc`
            // in `new`, and `mem_dc` is still live.
            unsafe {
                SelectObject(self.mem_dc, self.previous);
            }
            self.previous = 0;
        }
    }
}

impl Drop for DibSurface {
    fn drop(&mut self) {
        // Safety: each handle is either zero (never allocated, skip) or a value
        // this struct received from the matching creation call and has not yet
        // released. Release order is the reverse of acquisition, and restoring
        // the previous object before deleting the DC is what keeps the DC valid
        // while it still references our bitmap.
        unsafe {
            if self.previous != 0 && self.previous != HGDI_ERROR {
                SelectObject(self.mem_dc, self.previous);
            }
            if self.bitmap != 0 {
                DeleteObject(self.bitmap);
            }
            if self.mem_dc != 0 {
                DeleteDC(self.mem_dc);
            }
            if self.screen_dc != 0 {
                ReleaseDC(0, self.screen_dc);
            }
        }
    }
}

/// Current bounds of a window, in screen pixels.
fn window_rect(hwnd: isize) -> io::Result<Rect> {
    let mut raw = WinRect::default();
    if unsafe { GetWindowRect(hwnd, &mut raw) } == 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("GetWindowRect failed for window {hwnd}: {error}"),
        ));
    }
    Ok(Rect {
        left: raw.left,
        top: raw.top,
        right: raw.right,
        bottom: raw.bottom,
    })
}

/// Photograph `hwnd` at its current size, as it paints itself.
///
/// Returns the pixels without judging them: a blank capture is still a capture,
/// and the caller that wants the judgement asks for [`capture_window_to_png`].
/// Keeping the two apart is what lets a diagnostic path save the evidence of a
/// blank frame instead of losing it to the error.
pub fn capture_window(hwnd: isize) -> io::Result<Capture> {
    if hwnd == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "0 is not a window handle: it is the value GetForegroundWindow uses for 'none'",
        ));
    }
    let rect = window_rect(hwnd)?;
    if !rect.has_positive_span() {
        return Err(io::Error::other(format!(
            "window {hwnd} has no paintable area ({}x{})",
            rect.width(),
            rect.height()
        )));
    }
    let (width, height) = (rect.width(), rect.height());
    let mut surface = DibSurface::new(width, height)?;

    let painted = unsafe { PrintWindow(hwnd, surface.mem_dc, PW_RENDERFULLCONTENT) };
    if painted == 0 {
        return Err(io::Error::other(format!(
            "PrintWindow did not paint window {hwnd} ({width}x{height})"
        )));
    }

    // The bitmap was selected into `mem_dc` for PrintWindow to paint into.
    // GetDIBits reads it back and requires it not to be selected, so put the
    // DC's previous object back first.
    surface.deselect();

    let mut buffer = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut info = BitmapInfo {
        header: BitmapInfoHeader {
            size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            width,
            // Negative height asks for a TOP-DOWN buffer, so row 0 is the top
            // of the window. The positive form is bottom-up, and every consumer
            // of this buffer indexes it as a grid: getting it wrong flips the
            // image and stays invisible until somebody reads text in it.
            height: -height,
            planes: 1,
            bit_count: 32,
            compression: BI_RGB,
            size_image: 0,
            x_pels_per_meter: 0,
            y_pels_per_meter: 0,
            clr_used: 0,
            clr_important: 0,
        },
        colors: [RgbQuad::default(); 1],
    };
    let copied = unsafe {
        GetDIBits(
            surface.mem_dc,
            surface.bitmap,
            0,
            height as u32,
            buffer.as_mut_ptr(),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    // Fewer lines than the window is tall means the buffer's tail is still the
    // zeroes it was allocated with, which would read as a black band: refuse
    // rather than return a frame with a plausible-looking hole in it.
    if copied != height {
        return Err(io::Error::other(format!(
            "GetDIBits copied {copied} of {height} scan lines for window {hwnd}"
        )));
    }

    bgra_to_rgba(&mut buffer);
    Capture::from_rgba(width as u32, height as u32, buffer).ok_or_else(|| {
        io::Error::other(format!(
            "capture buffer does not describe {width}x{height} RGBA"
        ))
    })
}

/// Photograph `hwnd`, write it to `path` as PNG, and fail if it came out blank.
///
/// The blank check is the reason this function exists next to
/// [`capture_window`]: a caller that writes a file and reports "captured" has
/// proved nothing about the image, and `PrintWindow` will happily hand back a
/// black rectangle for a window it failed to paint.
///
/// The PNG is written *before* the blank check fails, so the evidence survives
/// the failure and the CI job's `if: always()` upload still carries it.
pub fn capture_window_to_png(hwnd: isize, path: &Path) -> io::Result<Capture> {
    let capture = capture_window(hwnd)?;
    let png = capture
        .to_png()
        .map_err(|e| io::Error::other(format!("PNG encode failed: {e}")))?;
    std::fs::write(path, &png)
        .map_err(|e| io::Error::new(e.kind(), format!("cannot write {}: {e}", path.display())))?;
    if capture.is_blank() {
        return Err(io::Error::other(format!(
            "window {hwnd} captured blank: ink ratio {:.6} over {}x{} (wrote {})",
            capture.ink_ratio,
            capture.width,
            capture.height,
            path.display()
        )));
    }
    Ok(capture)
}
