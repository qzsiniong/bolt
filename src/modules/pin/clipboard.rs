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

    // 富文本优先于 plain-text：浏览器/Safari 复制时两者都有，HTML 里的
    // 段落、列表、换行结构比 plain-text 更完整（plain-text 常是一整行）。
    let html_key = ns_string("public.html");
    if !html_key.is_null() {
        let raw: *mut AnyObject = unsafe { msg_send![pb, stringForType: html_key] };
        if let Some(html) = ns_string_to_rust(raw) {
            let text = html_to_text(&html);
            if !text.trim().is_empty() {
                console.info(format!(
                    "pin 来源 HTML（已转纯文本，{} 字）",
                    text.chars().count()
                ));
                return Some(PinContent::Text(text));
            }
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

/// 剪切板 HTML → 纯文本。
///
/// GPUI 没有 HTML 渲染能力，这里只做「保留结构」的提取：
/// - 丢弃 `<script>` / `<style>` 的内容；
/// - 块级标签（p/div/h1-6/li/br/tr/pre/blockquote）转成换行，`<li>` 加 `• `；
/// - `<pre>` 内保留原始空白，其余位置折叠连续空白；
/// - 解码常见实体（`&amp;` `&nbsp;` `&#39;` `&#x27;` 等）；
/// - 行尾空白与多余空行清理掉。
fn html_to_text(html: &str) -> String {
    let mut raw = String::with_capacity(html.len());
    let mut in_pre = false;
    let mut in_skip = false;
    let mut tag = String::new();
    let mut chars = html.chars();

    while let Some(c) = chars.next() {
        if c != '<' {
            if in_skip {
                continue;
            }
            if in_pre {
                raw.push(c);
            } else if c.is_whitespace() {
                // 折叠空白，但不要把行首的换行前面再塞空格。
                if !raw.ends_with(' ') && !raw.ends_with('\n') && !raw.is_empty() {
                    raw.push(' ');
                }
            } else {
                raw.push(c);
            }
            continue;
        }

        tag.clear();
        let mut closed = false;
        for c2 in chars.by_ref() {
            if c2 == '>' {
                closed = true;
                break;
            }
            tag.push(c2);
        }
        if !closed {
            break;
        }

        let is_closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .trim()
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        match name.as_str() {
            "script" | "style" => in_skip = !is_closing,
            "pre" => in_pre = !is_closing,
            "br" => raw.push('\n'),
            "li" => {
                trim_trailing_spaces(&mut raw);
                if !raw.is_empty() && !raw.ends_with('\n') {
                    raw.push('\n');
                }
                if !is_closing {
                    raw.push_str("• ");
                }
            }
            "p" | "div" | "section" | "article" | "blockquote" | "tr" | "table" | "h1" | "h2"
            | "h3" | "h4" | "h5" | "h6" => {
                trim_trailing_spaces(&mut raw);
                if !raw.is_empty() && !raw.ends_with('\n') {
                    raw.push('\n');
                }
            }
            "td" | "th" => raw.push(' '),
            _ => {}
        }
    }

    let decoded = decode_entities(&raw);
    let mut out = String::with_capacity(decoded.len());
    let mut blank_lines = 0;
    for line in decoded.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

fn trim_trailing_spaces(text: &mut String) {
    while text.ends_with(' ') {
        text.pop();
    }
}

/// 解码 HTML 实体：命名的常见几个 + `&#NN;` / `&#xNN;`。
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity.strip_prefix('#').and_then(|num| {
                let code = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                };
                code.and_then(char::from_u32)
            }),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
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
    fn html_becomes_readable_text() {
        // 浏览器复制的富文本：段落/列表/换行结构要保留，脚本与样式要丢掉。
        let html = r#"<html><head><style>p { color: red; }</style></head><body>
    <h1>标题</h1><p>第一段 &amp; 第二段<br>换行</p>
    <script>console.log(1)</script>
    <ul><li>甲</li><li>乙</li></ul>
    <pre>  a
  b</pre></body></html>"#;
        let text = html_to_text(html);
        assert!(text.starts_with("标题"), "{text:?}");
        assert!(text.contains("第一段 & 第二段"), "{text:?}");
        assert!(text.contains("换行"), "{text:?}");
        assert!(!text.contains("color: red"), "{text:?}");
        assert!(!text.contains("console"), "{text:?}");
        assert!(text.contains("• 甲"), "{text:?}");
        assert!(text.contains("• 乙"), "{text:?}");
        // `<pre>` 内的原始空白与换行要保留。
        assert!(text.contains("  a\n  b"), "{text:?}");
    }

    #[test]
    fn html_entities_decode() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(decode_entities("A&nbsp;B"), "A B");
        assert_eq!(decode_entities("&#39;引号&#x4e2d;"), "'引号中");
        assert_eq!(decode_entities("100 & 200"), "100 & 200");
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
