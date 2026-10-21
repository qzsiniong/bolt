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
pub(crate) fn icon_bytes(app: &Path) -> Result<Vec<u8>, String> {
    if let Some(bytes) = load_cached(app) {
        return Ok(bytes);
    }
    let png = generate_icon(app)?;
    store_cached(app, &png);
    Ok(png)
}

/// `NSWorkspace iconForFile:` → 最大的位图 representation → AppKit 转 PNG。
#[cfg(target_os = "macos")]
fn generate_icon(app: &Path) -> Result<Vec<u8>, String> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};

    use crate::ui::mac_ffi::{ns_data_bytes, ns_string};

    autoreleasepool(|_| unsafe {
        let workspace_class =
            AnyClass::get(c"NSWorkspace").ok_or("找不到 NSWorkspace 类".to_string())?;
        let workspace: *mut AnyObject = msg_send![workspace_class, sharedWorkspace];
        if workspace.is_null() {
            return Err("sharedWorkspace 返回空".to_string());
        }
        let path = ns_string(&app.to_string_lossy());
        if path.is_null() {
            return Err("路径 NSString 创建失败".to_string());
        }
        let image: *mut AnyObject = msg_send![workspace, iconForFile: path];
        if image.is_null() {
            return Err("iconForFile 返回空".to_string());
        }

        // 现代 macOS 的 `TIFFRepresentation` 是 16 位浮点 TIFF（linear 色彩空间），
        // Rust 的 image crate 解不了（Unhandled TIFF sample format 3 for 16 bits）。
        // 所以 TIFF 只当中间格式：全部位图 rep 交给 AppKit 自己解码
        // （imageRepsWithData 保证返回的都是 NSBitmapImageRep），选最大的一张，
        // 再由 AppKit 转成 8 位 PNG（representationUsingType：NSPNGFileType = 4）。
        let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
        if tiff.is_null() {
            return Err("TIFFRepresentation 为空".to_string());
        }
        let bitmap_class =
            AnyClass::get(c"NSBitmapImageRep").ok_or("找不到 NSBitmapImageRep 类".to_string())?;
        let reps: *mut AnyObject = msg_send![bitmap_class, imageRepsWithData: tiff];
        if reps.is_null() {
            return Err("imageRepsWithData 返回空".to_string());
        }
        let count: usize = msg_send![reps, count];
        let mut best: *mut AnyObject = std::ptr::null_mut();
        let mut best_pixels = 0usize;
        for i in 0..count {
            let rep: *mut AnyObject = msg_send![reps, objectAtIndex: i];
            if rep.is_null() {
                continue;
            }
            let w: usize = msg_send![rep, pixelsWide];
            let h: usize = msg_send![rep, pixelsHigh];
            if w.saturating_mul(h) > best_pixels {
                best_pixels = w * h;
                best = rep;
            }
        }
        if best.is_null() {
            return Err("图标没有任何位图 representation".to_string());
        }
        let nil = std::ptr::null_mut::<AnyObject>();
        let png_data: *mut AnyObject =
            msg_send![best, representationUsingType: 4usize, properties: nil];
        let png = ns_data_bytes(png_data).ok_or("PNG 转换失败".to_string())?;

        // 统一缩到 64×64：PNG 已是 8 位，image crate 可以正常解码。
        let decoded = image::load_from_memory(&png)
            .map_err(|e| format!("PNG 解码失败（{} 字节）：{e}", png.len()))?
            .to_rgba8();
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
            .map_err(|e| format!("PNG 编码失败：{e}"))?;
        Ok(out)
    })
}

#[cfg(not(target_os = "macos"))]
fn generate_icon(_app: &Path) -> Result<Vec<u8>, String> {
    Err("仅 macOS 支持".to_string())
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
