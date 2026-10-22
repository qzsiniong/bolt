//! 应用图标：优先读磁盘缓存，缺失时用 `NSWorkspace` 取系统图标并转成 PNG。
//!
//! 取图标这一步必须是**廉价**的：`iconForFile:` 返回的 `NSImage` 带三十多份
//! representation，旧实现用 `TIFFRepresentation` 把它们全部序列化，单个图标
//! 的 TIFF 有 40–75MB、耗时 300–600ms。首次启动要为上百个应用生成图标，主
//! 线程会被占满几十秒，全局快捷键的事件 tap 等不到回调会被系统判定超时禁用，
//! 表现就是「按 Alt+Space 没反应 / Launcher 不出来」。
//!
//! 这里改成让 AppKit 直接给出 64pt 的 `CGImage`（Retina 下 128px），再交给
//! `NSBitmapImageRep` 编成 PNG：release 下单个图标只要几毫秒，整批上百个应用
//! 也就一两秒。调用方仍需分批让出主线程，见 [`super::AppsModule::new`]。
//!
//! `NSWorkspace` 是 AppKit，**只能在主线程调用**（调用方负责线程约束）；
//! 本文件其余部分（缓存路径、读写、校验）是纯文件系统操作。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// 图标统一缩放到 64×64——列表里只显示 16pt，64 足够覆盖 2x/3x 屏。
const ICON_SIZE: u32 = 64;

fn cache_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
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
    let bytes = std::fs::read(cache_path(app)).ok()?;
    usable_icon(&bytes).then_some(bytes)
}

fn store_cached(app: &Path, png: &[u8]) {
    let path = cache_path(app);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, png);
}

/// 缓存/生成的字节必须真的能显示。
///
/// 之前有一批 233 字节的全透明 PNG 被写进缓存（AppKit 直接吐出来的空图标），
/// 之后每次启动都直接拿来用，列表里的应用图标永远是空白。这里把无法解码、
/// 尺寸不对、整张透明的 PNG 一律当成「没有缓存」，让调用方重新生成。
fn usable_icon(png: &[u8]) -> bool {
    let Ok(decoded) = image::load_from_memory(png) else {
        return false;
    };
    if decoded.width() != ICON_SIZE || decoded.height() != ICON_SIZE {
        return false;
    }
    decoded.to_rgba8().pixels().any(|pixel| pixel.0[3] != 0)
}

/// 取图标的 PNG 字节：缓存优先，缺失时生成并写回缓存。
pub(crate) fn icon_bytes(app: &Path) -> Result<Vec<u8>, String> {
    if let Some(bytes) = load_cached(app) {
        return Ok(bytes);
    }
    let png = generate_icon(app)?;
    if !usable_icon(&png) {
        return Err("生成的图标为空或全透明".to_string());
    }
    store_cached(app, &png);
    Ok(png)
}

/// 只定义本次调用用到的 CoreGraphics 结构体，并按系统 `@encode` 实现编码；
/// objc2 在 debug 构建下会校验方法签名，编码不一致会直接 panic。
#[cfg(target_os = "macos")]
mod cg {
    use objc2::encode::{Encode, Encoding, RefEncode};

    #[repr(C)]
    pub(super) struct CGPoint {
        pub(super) x: f64,
        pub(super) y: f64,
    }

    // SAFETY: `repr(C)`，字段布局与 `CGPoint`（两个 CGFloat）一致。
    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }

    #[repr(C)]
    pub(super) struct CGSize {
        pub(super) width: f64,
        pub(super) height: f64,
    }

    // SAFETY: `repr(C)`，字段布局与 `CGSize`（两个 CGFloat）一致。
    unsafe impl Encode for CGSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }

    #[repr(C)]
    pub(super) struct CGRect {
        pub(super) origin: CGPoint,
        pub(super) size: CGSize,
    }

    // SAFETY: `repr(C)`，字段布局与 `CGRect` 一致。
    unsafe impl Encode for CGRect {
        const ENCODING: Encoding =
            Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }

    // SAFETY: `CGRect` 的指针编码是 `^{CGRect=...}`，与系统头文件一致。
    unsafe impl RefEncode for CGRect {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }

    /// `CGImageRef` 的类型标记，只用来接收/传递指针，从不构造。
    #[repr(C)]
    pub(super) struct CGImage {
        _priv: [u8; 0],
    }

    // SAFETY: `CGImageRef` 的编码是 `^{CGImage=}`，与系统头文件一致。
    unsafe impl RefEncode for CGImage {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
    }
}

/// `NSWorkspace iconForFile:` → 64pt `CGImage` → `NSBitmapImageRep` → PNG。
///
/// 不要再改回 `TIFFRepresentation`：它会把 `NSImage` 的全部 representation
/// 序列化，单价是数百毫秒、几十 MB 内存。
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

        // 传入期望尺寸，AppKit 自己挑/画一份 64pt 的位图（Retina 下 128px）。
        let mut proposed = cg::CGRect {
            origin: cg::CGPoint { x: 0.0, y: 0.0 },
            size: cg::CGSize {
                width: f64::from(ICON_SIZE),
                height: f64::from(ICON_SIZE),
            },
        };
        let nil = std::ptr::null_mut::<AnyObject>();
        let cg_image: *mut cg::CGImage = msg_send![
            image,
            CGImageForProposedRect: &mut proposed as *mut cg::CGRect,
            context: nil,
            hints: nil
        ];
        if cg_image.is_null() {
            return Err("CGImageForProposedRect 返回空".to_string());
        }

        let bitmap_class =
            AnyClass::get(c"NSBitmapImageRep").ok_or("找不到 NSBitmapImageRep 类".to_string())?;
        let rep: *mut AnyObject = msg_send![bitmap_class, alloc];
        let rep: *mut AnyObject = msg_send![rep, initWithCGImage: cg_image];
        if rep.is_null() {
            return Err("initWithCGImage 返回空".to_string());
        }
        // alloc/init 是 +1 所有权；丢进当前 autoreleasepool，出池即回收。
        let rep: *mut AnyObject = msg_send![rep, autorelease];

        // NSPNGFileType = 4。
        let png_data: *mut AnyObject =
            msg_send![rep, representationUsingType: 4usize, properties: nil];
        let png = ns_data_bytes(png_data).ok_or("PNG 转换失败".to_string())?;

        // AppKit 给的是 128×128（64pt @2x），统一缩到 64×64 再入缓存。
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
    use image::ImageEncoder as _;

    fn png_of(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let pixels: Vec<u8> = vec![rgba; (width * height) as usize]
            .into_iter()
            .flatten()
            .collect();
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

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

    #[test]
    fn opaque_icon_is_usable() {
        assert!(usable_icon(&png_of(
            ICON_SIZE,
            ICON_SIZE,
            [10, 20, 30, 255]
        )));
    }

    /// 回归：曾经有一批全透明的 PNG 被写进缓存，之后怎么重启图标都是空白。
    #[test]
    fn fully_transparent_icon_is_not_usable() {
        assert!(!usable_icon(&png_of(ICON_SIZE, ICON_SIZE, [0, 0, 0, 0])));
    }

    #[test]
    fn wrong_size_or_garbage_is_not_usable() {
        assert!(!usable_icon(&png_of(32, 32, [10, 20, 30, 255])));
        assert!(!usable_icon(b"not a png"));
        assert!(!usable_icon(&[]));
    }
}
