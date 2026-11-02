//! `scanQr` 背后的截屏与二维码解码。
//!
//! 截屏语义沿用 clipbeam：枚举所有显示器，逐块抓图、转灰度，超宽屏先降采样，
//! 然后解出第一个能读通的二维码。返回的文本是什么意思由调用方决定——本模块
//! 只负责把它取出来。

use image::GrayImage;
use image::imageops::{FilterType, grayscale, resize};
use rqrr::PreparedImage;
use xcap::Monitor;

use crate::runtime::interaction::InteractionBridge;

/// 解码前的宽度上限。二维码边缘硬朗，最近邻降采样后照样能读，又能让大屏上的
/// 解码耗时保持稳定。
const MAX_CAPTURE_WIDTH: u32 = 1600;

/// 本进程是否有截屏权限。缺权限时顺带向系统申请一次。取色器模块共用。
///
/// 只有 macOS 有「屏幕录制」这道显式授权；Windows 的截屏走的是普通桌面访问，
/// 不存在单独开关。
#[cfg(target_os = "macos")]
pub(crate) fn ensure_capture_access() -> bool {
    use core_graphics::access::ScreenCaptureAccess;
    let access = ScreenCaptureAccess;
    access.preflight() || access.request()
}

#[cfg(target_os = "windows")]
pub(crate) fn ensure_capture_access() -> bool {
    true
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn ensure_capture_access() -> bool {
    false
}

#[cfg(target_os = "macos")]
const CAPTURE_ACCESS_HINT: &str = "识别二维码需要「屏幕录制」权限：请在「系统设置 → 隐私与安全性 → 屏幕录制」中允许 Bolt（修改后需重启应用）";

#[cfg(not(target_os = "macos"))]
const CAPTURE_ACCESS_HINT: &str = "当前系统没有授予 Bolt 截屏能力";

/// `scanQr()`：解出第一个二维码返回 `Ok(Some(text))`，所有屏幕都没有则
/// `Ok(None)`。
///
/// 不显示进度条：截屏 + 解码足够快，不值得为它开窗；而且那个进度浮窗会抢走用户
/// 目标窗口的焦点。
pub fn scan_qr(_bridge: &InteractionBridge) -> Result<Option<String>, String> {
    if !ensure_capture_access() {
        return Err(CAPTURE_ACCESS_HINT.to_string());
    }
    scan_screens()
}

fn scan_screens() -> Result<Option<String>, String> {
    let monitors = Monitor::all().map_err(|e| format!("枚举显示器失败：{e}"))?;
    for monitor in monitors {
        // 单块屏幕可能瞬时失败（热插拔、显示器休眠），跳过它就行，不必让整轮
        // 扫描失败。
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

/// 返回画面里第一个能解出的二维码。一屏可能同时有多个码；返回哪一个不确定。
fn decode(gray: GrayImage) -> Option<String> {
    let mut prepared = PreparedImage::prepare(gray);
    for grid in prepared.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            return Some(content);
        }
    }
    None
}
