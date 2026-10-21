//! 剪切板读取：NSPasteboard → [`PinContent`]。
//!
//! macOS 上直接发原始 Objective-C 消息（objc2 0.6 的 `msg_send!`）：
//! `[NSPasteboard generalPasteboard]` 拿共享剪切板，按优先级尝试图片
//! （`public.png` / `public.tiff` / `public.jpeg` / `public.gif`）、文本
//! （`public.utf8-plain-text`）。不用 `objc2-app-kit` / `objc2-foundation`
//! 是因为它们目前依赖 objc2 0.5，会与本项目的 objc2 0.6 同时进入依赖图。
//! 图片字节再喂给 `image` 拿尺寸，窗口初始 bounds 用得到。

use std::path::PathBuf;

use crate::console_channel::ConsoleChannel;

/// pin 便签展示的最大尺寸（逻辑像素）。
pub(crate) const MAX_PIN_WIDTH: f32 = 800.;
pub(crate) const MAX_PIN_HEIGHT: f32 = 600.;

/// 剪切板里读到的内容类型。
pub enum PinContent {
    Image {
        /// **PNG** 字节：非 PNG 的输入（TIFF/JPEG/GIF）在读取时就统一转码了，
        /// 渲染端可以无条件按 `ImageFormat::Png` 解码。
        bytes: Vec<u8>,
        /// 原始图像尺寸（像素），窗口初始大小据此裁剪。
        width: u32,
        height: u32,
    },
    Text(String),
}

impl PinContent {
    /// 便签展示尺寸：窗口 bounds 与图片元素共用这一份，避免窗口大、图片 0×0。
    ///
    /// 图片按比例缩进上限内（不放大）；文本给定宽、按字符数粗略估高。
    pub(crate) fn display_size(&self) -> (f32, f32) {
        match self {
            PinContent::Image { width, height, .. } => {
                let (w, h) = (*width as f32, *height as f32);
                let scale = (MAX_PIN_WIDTH / w).min(MAX_PIN_HEIGHT / h).min(1.);
                (w * scale, h * scale)
            }
            PinContent::Text(text) => {
                // 固定最大宽度 320px，高度按字符数粗略估（每个中文 ~14px 宽）。
                let w = 320.;
                let chars = text.chars().count();
                let lines = (chars as f32 * 14. / w).max(1.);
                (w, (lines * 20. + 24.).min(MAX_PIN_HEIGHT))
            }
        }
    }
}

/// 默认另存为目录：用户桌面。
pub(crate) fn default_save_dir() -> PathBuf {
    dirs::desktop_dir().unwrap_or_else(|| PathBuf::from("~"))
}

// ── 平台分派 ──────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub fn read_clipboard(console: &ConsoleChannel) -> Option<PinContent> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    let pb: *mut AnyObject = unsafe {
        let cls = AnyClass::get(c"NSPasteboard")?;
        msg_send![cls, generalPasteboard]
    };
    if pb.is_null() {
        return None;
    }

    let file_paths = pasteboard_file_paths(pb);
    if let Some(path) = file_paths.first() {
        // 剪贴板是「文件」形态（Finder 里 Cmd+C）。此时位图类型（public.png /
        // public.tiff）往往是系统给这个文件画的**图标**（1024×1024），不是图片
        // 内容本身，绝不能当图片 pin。所以只认文件本身，不再回退位图。
        match read_image_file(path) {
            Ok((w, h, png)) => {
                console.info(format!("pin 来源文件：{}（{w}×{h}）", path.display()));
                return Some(PinContent::Image {
                    bytes: png,
                    width: w,
                    height: h,
                });
            }
            Err(e) => console.warn(format!("剪切板是文件 {}：{e}", path.display())),
        }
        // 文件形态下不要落到文本分支：Finder 同时把文件名（如 `1995.jpg`）
        // 写进 plain-text 类型，pin 出来会是一个只有文件名的文本便签。
        return None;
    }

    for uti in ["public.png", "public.tiff", "public.jpeg", "public.gif"] {
        let uti_obj = ns_string(uti);
        if uti_obj.is_null() {
            continue;
        }
        let data: *mut AnyObject = unsafe { msg_send![pb, dataForType: uti_obj] };
        let Some(bytes) = ns_data_bytes(data) else {
            continue;
        };

        // 统一成 PNG：渲染端固定按 PNG 解码，原样透传 JPEG/GIF 会解不出来。
        let png = if is_png(&bytes) {
            bytes
        } else {
            match convert_to_png(&bytes) {
                Ok(b) => b,
                Err(_) => continue,
            }
        };

        if let Some((w, h)) = image_dimensions(&png) {
            console.info(format!("pin 来源位图：{uti}（{w}×{h}）"));
            return Some(PinContent::Image {
                bytes: png,
                width: w,
                height: h,
            });
        }
    }

    let utf8_key = ns_string("public.utf8-plain-text");
    if !utf8_key.is_null() {
        let nsstr: *mut AnyObject = unsafe { msg_send![pb, stringForType: utf8_key] };
        if let Some(text) = ns_string_to_rust(nsstr)
            && !text.trim().is_empty()
        {
            return Some(PinContent::Text(text));
        }
    }

    None
}

/// 建一个 autoreleased 的 `NSString`（`+[NSString stringWithUTF8String:]`）。
/// 只在同一次调用里当类型/键名用，不长期持有。
#[cfg(target_os = "macos")]
fn ns_string(s: &str) -> *mut objc2::runtime::AnyObject {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use std::ffi::CString;

    let Ok(cstr) = CString::new(s) else {
        return std::ptr::null_mut();
    };
    let Some(cls) = AnyClass::get(c"NSString") else {
        return std::ptr::null_mut();
    };
    unsafe { msg_send![cls, stringWithUTF8String: cstr.as_ptr()] }
}

/// `NSData*` → `Vec<u8>`（立刻拷贝，不持有 Objective-C 对象）。
#[cfg(target_os = "macos")]
fn ns_data_bytes(data: *mut objc2::runtime::AnyObject) -> Option<Vec<u8>> {
    use objc2::msg_send;

    if data.is_null() {
        return None;
    }
    unsafe {
        let len: usize = msg_send![data, length];
        // `-[NSData bytes]` 的 ObjC 返回类型是 `const void *`，编码 `^v`；
        // objc2 在 debug 下会校验声明类型，声明成 `*const u8`（编码 `*`）会在
        // 运行时直接 abort：invalid message send ... expected '^v', but found '*'。
        // 所以必须先按 `*const c_void` 收，再自己转成 `*const u8`。
        let ptr: *const std::ffi::c_void = msg_send![data, bytes];
        if ptr.is_null() || len == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr as *const u8, len).to_vec())
    }
}

/// `NSString*` → `String`（走 UTF8String，拷贝后即与 Objective-C 对象无关）。
#[cfg(target_os = "macos")]
fn ns_string_to_rust(nsstr: *mut objc2::runtime::AnyObject) -> Option<String> {
    use objc2::msg_send;

    if nsstr.is_null() {
        return None;
    }
    unsafe {
        let ptr: *const std::ffi::c_char = msg_send![nsstr, UTF8String];
        if ptr.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn read_clipboard(_console: &ConsoleChannel) -> Option<PinContent> {
    None
}

/// 剪切板里指向的文件（Finder 里 Cmd+C 复制文件）。
///
/// 同一份信息可能有三种形态，逐级兜底：
/// 1. `public.file-url` 按字符串读；
/// 2. `public.file-url` 只能按原始数据读（`stringForType:` 返回 nil 时）；
/// 3. 老式 `NSFilenamesPboardType` 的 plist 数组。
#[cfg(target_os = "macos")]
fn pasteboard_file_paths(pb: *mut objc2::runtime::AnyObject) -> Vec<PathBuf> {
    use objc2::msg_send;

    let mut paths = Vec::new();

    let url_key = ns_string("public.file-url");
    if !url_key.is_null() {
        let raw: *mut objc2::runtime::AnyObject = unsafe { msg_send![pb, stringForType: url_key] };
        if let Some(path) = ns_string_to_rust(raw).and_then(|url| file_url_to_path(&url)) {
            paths.push(path);
        }
        if paths.is_empty() {
            let data: *mut objc2::runtime::AnyObject =
                unsafe { msg_send![pb, dataForType: url_key] };
            if let Some(bytes) = ns_data_bytes(data)
                && let Some(path) = file_url_to_path(&String::from_utf8_lossy(&bytes))
            {
                paths.push(path);
            }
        }
    }

    if paths.is_empty() {
        let key = ns_string("NSFilenamesPboardType");
        if !key.is_null() {
            let data: *mut objc2::runtime::AnyObject = unsafe { msg_send![pb, dataForType: key] };
            if let Some(bytes) = ns_data_bytes(data) {
                for path in plist_strings(&String::from_utf8_lossy(&bytes)) {
                    paths.push(PathBuf::from(path));
                }
            }
        }
    }

    paths
}

/// `file:///Users/…` / `file:///.file/id=…` → `PathBuf`。
///
/// Finder 现代复制给的是**文件引用 URL**（`file:///.file/id=6571367.1534725`），
/// 那不是真实路径，直接 `fs::read` 会报 "Not a directory"。必须交给 `NSURL`
/// 解析：`filePathURL` 把引用 URL 变成路径 URL，`path` 再取出真实路径。
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Some(path) = ns_url_path(url.trim()) {
        return Some(path);
    }
    // 兜底：普通 `file://` URL（NSURL 不可用时仍能解出绝大多数情况）。
    Some(PathBuf::from(percent_decode(
        url.trim().strip_prefix("file://")?,
    )))
}

/// 借 `NSURL` 把文件 URL（含文件引用 URL、百分号转义）还原成真实路径。
#[cfg(target_os = "macos")]
fn ns_url_path(url_string: &str) -> Option<PathBuf> {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

    unsafe {
        let cls = AnyClass::get(c"NSURL")?;
        let s = ns_string(url_string);
        if s.is_null() {
            return None;
        }
        let url: *mut objc2::runtime::AnyObject = msg_send![cls, URLWithString: s];
        if url.is_null() {
            return None;
        }
        // `filePathURL` 返回的仍是 NSURL；非文件 URL 时返回 nil，退回原对象。
        let file_url: *mut objc2::runtime::AnyObject = msg_send![url, filePathURL];
        let target = if file_url.is_null() { url } else { file_url };
        let path: *mut objc2::runtime::AnyObject = msg_send![target, path];
        ns_string_to_rust(path)
            .filter(|p| p.starts_with('/'))
            .map(PathBuf::from)
    }
}

/// 从 `NSFilenamesPboardType` 那种简单 plist 里取出所有 `<string>`。
fn plist_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<string>") {
        let after = &rest[start + "<string>".len()..];
        let Some(end) = after.find("</string>") else {
            break;
        };
        out.push(xml_unescape(&after[..end]));
        rest = &after[end + "</string>".len()..];
    }
    out
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 读一个本地图片文件 → (宽, 高, PNG 字节)。
///
/// 返回具体的失败原因：文件形态下不能静默失败去回退图标位图，得让用户看到
/// 到底是没权限还是文件不是图片。
fn read_image_file(path: &std::path::Path) -> Result<(u32, u32, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取失败：{e}"))?;
    let (w, h) = image_dimensions(&bytes).ok_or_else(|| "不是可解码的图片".to_string())?;
    let png = if is_png(&bytes) {
        bytes
    } else {
        convert_to_png(&bytes).map_err(|e| format!("转码 PNG 失败：{e}"))?
    };
    Ok((w, h, png))
}

/// `%E4%B8%AD` 这类文件 URL 转义还原成普通字符串。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

// ── 辅助：图片解码 / TIFF→PNG ─────────────────────────────────────────────

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let img = image::load_from_memory(bytes).ok()?;
    Some((img.width(), img.height()))
}

fn convert_to_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    use image::codecs::png::PngEncoder;
    use std::io::Cursor;

    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let mut out = Cursor::new(Vec::new());
    {
        let encoder = PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::Adaptive,
        );
        // `as_rgba8()` 只在原图**本来就是** RGBA8 时才返回 Some；剪切板里的
        // TIFF/JPEG/GIF 常见 8-bit/16-bit 的非 RGBA 格式，必须走 `to_rgba8()`。
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        encoder
            .write_image(
                rgba.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_measures_and_tiff_converts_to_png() {
        // 便签的图片渲染与窗口尺寸都依赖「字节能解码出尺寸」这条链，
        // 解不出来就会开成一个没有内容的空白窗口。这里用真编码出来的字节，
        // 覆盖剪切板最常见的两种形态：PNG 直接用、TIFF（macOS 截图格式）转换后用。
        use image::ImageEncoder as _;
        use std::io::Cursor;

        let mut rgba = image::RgbaImage::new(3, 2);
        rgba.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));

        let png = {
            let mut out = Cursor::new(Vec::new());
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(rgba.as_raw(), 3, 2, image::ExtendedColorType::Rgba8)
                .unwrap();
            out.into_inner()
        };
        assert_eq!(image_dimensions(&png), Some((3, 2)));

        let tiff = {
            let mut out = Cursor::new(Vec::new());
            image::codecs::tiff::TiffEncoder::new(&mut out)
                .write_image(rgba.as_raw(), 3, 2, image::ExtendedColorType::Rgba8)
                .unwrap();
            out.into_inner()
        };
        assert_eq!(image_dimensions(&tiff), Some((3, 2)));

        let converted = convert_to_png(&tiff).expect("TIFF 应能转成 PNG");
        assert_eq!(image_dimensions(&converted), Some((3, 2)));
    }

    #[test]
    fn read_image_file_decodes_png_and_normalizes_jpeg() {
        // 覆盖「复制图片文件」这条路：读盘得到的必须是 PNG（渲染端固定按 PNG
        // 解码），且尺寸取原图尺寸而不是文件图标的尺寸。
        use crate::test_support::TempDir;
        use image::ImageEncoder as _;
        use std::io::Cursor;

        let mut rgba = image::RgbaImage::new(3, 2);
        rgba.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let dir = TempDir::new("pin-image-file");

        let png_path = dir.0.join("a.png");
        let mut png = Cursor::new(Vec::new());
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(rgba.as_raw(), 3, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&png_path, png.into_inner()).unwrap();
        let (w, h, bytes) = read_image_file(&png_path).expect("PNG 应能读出");
        assert_eq!((w, h), (3, 2));
        assert!(is_png(&bytes));

        let jpeg_path = dir.0.join("a.jpg");
        let mut jpeg = Cursor::new(Vec::new());
        // JPEG 编码器只吃 RGB8，RGBA8 会直接 Unsupported。
        let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .write_image(rgb.as_raw(), 3, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::write(&jpeg_path, jpeg.into_inner()).unwrap();
        let (w, h, bytes) = read_image_file(&jpeg_path).expect("JPEG 应能读出");
        assert_eq!((w, h), (3, 2));
        // 关键：JPEG 必须在读取阶段就转成 PNG。
        assert!(is_png(&bytes));

        assert!(read_image_file(&dir.0.join("missing.png")).is_err());
    }

    #[test]
    fn display_size_scales_images_and_keeps_text_width() {
        // 小图不放大；大图按比例缩进 800×600 上限；文本固定 320 宽。
        let small = PinContent::Image {
            bytes: vec![],
            width: 400,
            height: 300,
        };
        assert_eq!(small.display_size(), (400., 300.));

        let large = PinContent::Image {
            bytes: vec![],
            width: 2400,
            height: 1200,
        };
        // scale = min(800/2400, 600/1200) = 1/3
        assert_eq!(large.display_size(), (800., 400.));

        let (w, h) = PinContent::Text("hello".to_string()).display_size();
        assert_eq!(w, 320.);
        assert!(h > 0.);
    }

    #[test]
    fn plist_strings_reads_finder_filenames() {
        // Finder 的 NSFilenamesPboardType 是简单 plist 数组。
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><array>
	<string>/tmp/a &amp; b.png</string>
	<string>/tmp/中文.jpg</string>
</array></plist>"#;
        assert_eq!(
            plist_strings(xml),
            vec!["/tmp/a & b.png".to_string(), "/tmp/中文.jpg".to_string()]
        );
        assert!(plist_strings("not a plist").is_empty());
    }

    #[test]
    fn percent_decode_restores_file_url_path() {
        assert_eq!(
            percent_decode("/Users/qzs/Desktop/a%20b%E4%B8%AD.png"),
            "/Users/qzs/Desktop/a b中.png"
        );
        // 非法转义原样保留，不能 panic。
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn png_magic_detection() {
        assert!(is_png(b"\x89PNG\r\n\x1a\nxxxx"));
        assert!(!is_png(b"\xff\xd8\xff\xe0"));
        assert!(!is_png(b""));
    }
}
