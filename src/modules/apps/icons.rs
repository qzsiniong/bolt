//! 应用图标：优先读磁盘缓存，缺失时用 `NSWorkspace` 取系统图标并转成 PNG。
//!
//! `NSWorkspace` 是 AppKit，**只能在主线程调用**（调用方负责线程约束）；
//! 本文件其余部分（缓存路径、读写）是纯文件系统操作。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// 图标统一缩放到 64×64——列表里只显示 16pt，64 足够覆盖 2x/3x 屏。
const ICON_SIZE: u32 = 64;

fn cache_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("scripter");
    path.push("app-icons");
    path
}

/// 缓存文件名由应用路径哈希决定：同一路径稳定，不同路径不会冲突。
pub(crate) fn cache_path(app: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    app.hash(&mut hasher);
    cache_dir().join(format!("{:016x}.png", hasher.finish()))
}

pub(crate) fn load_cached(app: &Path) -> Option<Vec<u8>> {
    std::fs::read(cache_path(app)).ok()
}

fn store_cached(app: &Path, png: &[u8]) {
    let path = cache_path(app);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, png);
}

/// 取图标的 PNG 字节：缓存优先，缺失时生成并写回缓存。
pub(crate) fn icon_bytes(app: &Path) -> Option<Vec<u8>> {
    if let Some(bytes) = load_cached(app) {
        return Some(bytes);
    }
    let png = generate_icon(app)?;
    store_cached(app, &png);
    Some(png)
}

/// `NSWorkspace iconForFile:` → `NSImage` → TIFF → 缩放 → PNG。
#[cfg(target_os = "macos")]
fn generate_icon(app: &Path) -> Option<Vec<u8>> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};

    use crate::ui::mac_ffi::{ns_data_bytes, ns_string};

    autoreleasepool(|_| unsafe {
        let workspace_class = AnyClass::get(c"NSWorkspace")?;
        let workspace: *mut AnyObject = msg_send![workspace_class, sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let path = ns_string(&app.to_string_lossy());
        if path.is_null() {
            return None;
        }
        let image: *mut AnyObject = msg_send![workspace, iconForFile: path];
        if image.is_null() {
            return None;
        }
        // 图标带多个分辨率的 representation，TIFFRepresentation 会全部打包，
        // 体积不小但解码一次即可；随后统一缩到 64×64 再编码 PNG。
        let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
        let tiff_bytes = ns_data_bytes(tiff)?;

        let decoded = image::load_from_memory(&tiff_bytes).ok()?.to_rgba8();
        let resized = image::imageops::resize(
            &decoded,
            ICON_SIZE,
            ICON_SIZE,
            image::imageops::FilterType::Lanczos3,
        );

        use image::ImageEncoder as _;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(
                resized.as_raw(),
                ICON_SIZE,
                ICON_SIZE,
                image::ExtendedColorType::Rgba8,
            )
            .ok()?;
        Some(out)
    })
}

#[cfg(not(target_os = "macos"))]
fn generate_icon(_app: &Path) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_stable_and_path_specific() {
        let safari = Path::new("/Applications/Safari.app");
        assert_eq!(cache_path(safari), cache_path(safari));
        assert_ne!(
            cache_path(safari),
            cache_path(Path::new("/Applications/Notes.app"))
        );
        assert_eq!(
            cache_path(safari).extension().and_then(|e| e.to_str()),
            Some("png")
        );
        assert!(cache_path(safari).starts_with(cache_dir()));
    }
}
