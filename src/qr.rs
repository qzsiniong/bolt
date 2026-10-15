//! Screen capture and QR decoding for `scanQr`.
//!
//! Mirrors clipbeam's capture semantics: enumerate every monitor, grab each
//! screen, greyscale it, downsample very wide screens, then decode the first QR
//! code that reads. The caller decides what the returned text means — this
//! module only produces it.

use core_graphics::access::ScreenCaptureAccess;
use image::GrayImage;
use image::imageops::{FilterType, grayscale, resize};
use rqrr::PreparedImage;
use xcap::Monitor;

use crate::interaction::InteractionBridge;

/// Width cap before decoding. QR codes are hard-edged, so a nearest-neighbour
/// downsample stays readable while keeping decode time flat on large displays.
const MAX_CAPTURE_WIDTH: u32 = 1600;

const CAPTURE_ACCESS_HINT: &str = "识别二维码需要「屏幕录制」权限：请在「系统设置 → 隐私与安全性 → 屏幕录制」中允许 Scripter（修改后需重启应用）";

/// Whether this process may capture the screen. Asks the system for the
/// permission when it is missing.
fn ensure_capture_access() -> bool {
    let access = ScreenCaptureAccess::default();
    access.preflight() || access.request()
}

/// `scanQr()`: `Ok(Some(text))` for the first decodable QR code, `Ok(None)`
/// when no screen had one.
///
/// No progress bar: capture + decode is fast enough to not warrant one, and
/// the progress-floating window would steal focus from the user's target.
pub fn scan_qr(_bridge: &InteractionBridge) -> Result<Option<String>, String> {
    if !ensure_capture_access() {
        return Err(CAPTURE_ACCESS_HINT.to_string());
    }
    scan_screens()
}

fn scan_screens() -> Result<Option<String>, String> {
    let monitors = Monitor::all().map_err(|e| format!("枚举显示器失败：{e}"))?;
    for monitor in monitors {
        // A screen can fail transiently (hot-plug, display asleep); skip it
        // rather than failing the whole scan.
        let Ok(rgba) = monitor.capture_image() else {
            continue;
        };
        if let Some(text) = decode(downsample(grayscale(&rgba))) {
            return Ok(Some(text));
        }
    }
    Ok(None)
}

fn downsample(gray: GrayImage) -> GrayImage {
    if gray.width() <= MAX_CAPTURE_WIDTH {
        return gray;
    }
    let height = (gray.height() as u64 * MAX_CAPTURE_WIDTH as u64 / gray.width() as u64) as u32;
    resize(&gray, MAX_CAPTURE_WIDTH, height.max(1), FilterType::Nearest)
}

/// First QR code in the frame that decodes. Several codes can be visible at
/// once; which one comes back is not specified.
fn decode(gray: GrayImage) -> Option<String> {
    let mut prepared = PreparedImage::prepare(gray);
    for grid in prepared.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            return Some(content);
        }
    }
    None
}
