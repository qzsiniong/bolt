//! 屏幕快照：所有显示器的整屏 RgbaImage，供放大镜网格与拾色读取。
//!
//! 坐标系说明：`CGEvent::location()`、xcap `Monitor::x()/y()`（其实现直接
//! 返回 `CGDisplayBounds` 的 origin）与 `CGDisplayBounds` 本身同处一个全局
//! 空间——主显示器左上角为原点，y 向下——所以直接做矩形包含判定即可，
//! 无需换算。

use core_graphics::display::{CGDirectDisplayID, CGDisplayBounds, CGGetActiveDisplayList};
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use gpui_kit::DisplayId;
use image::RgbaImage;
use xcap::Monitor;

use crate::host::modules::qr::ensure_capture_access;

const PERMISSION_HINT: &str = "屏幕取色需要「屏幕录制」权限：请在「系统设置 → 隐私与安全性 → 屏幕录制」中允许 Bolt（修改后需重启应用）";

/// 一块显示器的整屏快照，每个屏幕各开一个浮窗。
pub(crate) struct ScreenSnapshot {
    /// 该屏整幅物理像素（Retina 下尺寸 = 逻辑尺寸 × scale）。
    pub(crate) image: RgbaImage,
    /// 显示器逻辑原点（CG 全局坐标）与逻辑尺寸（点）。
    pub(crate) origin: (f64, f64),
    pub(crate) logical_size: (f64, f64),
    /// 物理像素 / 逻辑点的比值（Retina = 2）。
    pub(crate) scale: f32,
    /// 对齐到的 gpui 显示器（开窗定位用）；对不上时开窗回退主屏。
    pub(crate) display_id: Option<DisplayId>,
    /// 鼠标在该屏内的本地逻辑坐标；鼠标不在该屏时为 None（浮窗不显示面板）。
    pub(crate) mouse_local: Option<(f64, f64)>,
}

/// 截取所有显示器的快照。权限缺失时 `request()` 已弹系统授权框，
/// 返回 Err（本次中止，授权后下次生效）。
pub(crate) fn capture_all() -> Result<Vec<ScreenSnapshot>, String> {
    if !ensure_capture_access() {
        crate::ui::mac_ffi::notify("屏幕取色", PERMISSION_HINT);
        return Err("缺少屏幕录制权限".to_string());
    }

    // 鼠标全局坐标。CGEventSource 无需辅助功能权限。
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "无法创建 CGEventSource".to_string())?;
    let mouse = CGEvent::new(source)
        .map_err(|_| "无法读取鼠标位置".to_string())?
        .location();

    let monitors = Monitor::all().map_err(|e| format!("枚举显示器失败：{e}"))?;
    let mut snapshots = Vec::new();
    for monitor in monitors {
        // x()/y() 可能取失败（显示器热插拔的瞬态）；跳过而不是整体失败。
        let (Ok(mx), Ok(my)) = (monitor.x(), monitor.y()) else {
            continue;
        };
        let (Ok(w), Ok(h)) = (monitor.width(), monitor.height()) else {
            continue;
        };

        // 快照在浮窗打开之前截取，画面里不会包含浮窗本身。
        let image = match monitor.capture_image() {
            Ok(img) => img,
            Err(e) => {
                // 单屏失败容忍：跳过该屏，其余屏幕继续。
                crate::ui::mac_ffi::notify("屏幕取色", &format!("一块显示器截屏失败：{e}"));
                continue;
            }
        };

        snapshots.push(ScreenSnapshot {
            image,
            origin: (mx as f64, my as f64),
            logical_size: (w as f64, h as f64),
            scale: monitor.scale_factor().unwrap_or(1.0).max(0.5),
            display_id: matching_display_id(mx, my, w, h),
            mouse_local: contains(mouse.x, mouse.y, mx, my, w, h)
                .then_some((mouse.x - mx as f64, mouse.y - my as f64)),
        });
    }
    if snapshots.is_empty() {
        return Err("没有可用的显示器".to_string());
    }
    Ok(snapshots)
}

fn contains(px: f64, py: f64, mx: i32, my: i32, w: u32, h: u32) -> bool {
    px >= mx as f64 && px < (mx + w as i32) as f64 && py >= my as f64 && py < (my + h as i32) as f64
}

/// 用几何对齐把 xcap Monitor 映射回 gpui 的 DisplayId（两者底层都是
/// `CGDisplayBounds`，原点与尺寸完全相等即同一块屏）。
fn matching_display_id(mx: i32, my: i32, w: u32, h: u32) -> Option<DisplayId> {
    // 32 个的上限与 gpui 平台层代码的假设一致。
    let mut ids: Vec<CGDirectDisplayID> = Vec::with_capacity(32);
    let mut count = 0;
    // SAFETY: the buffer has the capacity passed in; `count` says how many
    // entries were written.
    if unsafe { CGGetActiveDisplayList(ids.capacity() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return None;
    }
    unsafe { ids.set_len(count as usize) };

    ids.into_iter()
        .find(|&id| {
            // SAFETY: FFI call with a valid active display id.
            let frame = unsafe { CGDisplayBounds(id) };
            frame.origin.x as i32 == mx
                && frame.origin.y as i32 == my
                && frame.size.width as u32 == w
                && frame.size.height as u32 == h
        })
        .map(|id| DisplayId::new(id as u64))
}
