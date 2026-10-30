//! 图片格式与尺寸识别：全程不转码、不做全图解码。
//!
//! macOS 剪贴板里的图片基本是 PNG/TIFF/JPEG/GIF 四种（gpui 原生解码），
//! TIFF→PNG 的整读+重编码是粘贴卡顿的主因——读取链只按 magic 识别格式、
//! 只读文件头解析尺寸，原始字节原样透传给渲染端。

use std::path::Path;

/// 只读文件头 16 字节按 magic 识别图片格式，不整读文件。
pub(super) fn image_file_format(path: &Path) -> Option<gpui_kit::ImageFormat> {
    use std::io::Read as _;

    let Ok(mut file) = std::fs::File::open(path) else {
        return None;
    };
    let mut head = [0u8; 16];
    let n = file.read(&mut head).unwrap_or(0);
    detect_image_format(&head[..n])
}

/// 按字节 magic 识别图片格式（png/jpeg/gif/tiff——macOS 剪贴板里的图片
/// 基本都在这四种里，gpui 原生解码）。
pub(super) fn detect_image_format(bytes: &[u8]) -> Option<gpui_kit::ImageFormat> {
    if is_png(bytes) {
        Some(gpui_kit::ImageFormat::Png)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(gpui_kit::ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF8") {
        Some(gpui_kit::ImageFormat::Gif)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(gpui_kit::ImageFormat::Tiff)
    } else {
        None
    }
}

/// 读一个本地图片文件 → (格式, 宽, 高, 原始字节)。**不转码**：gpui 原生
/// 解码 PNG/TIFF/JPEG/GIF，渲染与另存都用原格式；整读后只做 magic 识别
/// 和文件头尺寸解析，一个像素都不碰。
///
/// 返回具体的失败原因：文件形态下不能静默失败去回退图标位图，得让用户看到
/// 到底是没权限还是文件不是图片。
pub(super) fn read_image_file(
    path: &Path,
) -> Result<(gpui_kit::ImageFormat, u32, u32, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取失败：{e}"))?;
    let format = detect_image_format(&bytes).ok_or_else(|| "不是可解码的图片".to_string())?;
    let (w, h) = image_dimensions(&bytes).ok_or_else(|| "图片头解析失败".to_string())?;
    Ok((format, w, h, bytes))
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

/// 只读图片头拿宽高（不做全图解码）：读取链跑在 UI 线程（Cmd+V 的
/// on_paste 回调里），全图解码一张 retina 截图也要几百毫秒。
pub(super) fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_dimensions_measures_header_only() {
        // 便签的图片渲染与窗口尺寸都依赖「字节能解出头尺寸」这条链，解不出
        // 就会开成一个没有内容的空白窗口。这里用真编码出来的字节，覆盖剪切
        // 板最常见的两种形态：PNG 直接用、TIFF（macOS 截图格式）原样透传。
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
        // 读取链不再转码：TIFF 按 magic 识别出格式、头里解出尺寸即可。
        assert_eq!(image_dimensions(&tiff), Some((3, 2)));
        assert_eq!(
            detect_image_format(&tiff),
            Some(gpui_kit::ImageFormat::Tiff)
        );
        assert_eq!(detect_image_format(&png), Some(gpui_kit::ImageFormat::Png));
    }

    #[test]
    fn read_image_file_detects_format_and_size() {
        // 覆盖「复制图片文件」这条路：不转码，返回原格式字节 + 原图尺寸。
        use crate::test_support::TempDir;
        use image::ImageEncoder as _;
        use std::io::Cursor;

        let mut rgba = image::RgbaImage::new(3, 2);
        rgba.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let dir = TempDir::new("paste-image-file");

        let png_path = dir.0.join("a.png");
        let mut png = Cursor::new(Vec::new());
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(rgba.as_raw(), 3, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        let png_bytes = png.into_inner();
        std::fs::write(&png_path, &png_bytes).unwrap();
        let (format, w, h, bytes) = read_image_file(&png_path).expect("PNG 应能读出");
        assert_eq!(format, gpui_kit::ImageFormat::Png);
        assert_eq!((w, h), (3, 2));
        assert_eq!(bytes, png_bytes);

        let jpeg_path = dir.0.join("a.jpg");
        let mut jpeg = Cursor::new(Vec::new());
        // JPEG 编码器只吃 RGB8，RGBA8 会直接 Unsupported。
        let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .write_image(rgb.as_raw(), 3, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        let jpeg_bytes = jpeg.into_inner();
        std::fs::write(&jpeg_path, &jpeg_bytes).unwrap();
        let (format, w, h, bytes) = read_image_file(&jpeg_path).expect("JPEG 应能读出");
        assert_eq!(format, gpui_kit::ImageFormat::Jpeg);
        assert_eq!((w, h), (3, 2));
        // 关键：JPEG 原样透传，不做任何转码。
        assert_eq!(bytes, jpeg_bytes);

        assert!(read_image_file(&dir.0.join("missing.png")).is_err());
    }

    #[test]
    fn png_magic_detection() {
        assert!(is_png(b"\x89PNG\r\n\x1a\nxxxx"));
        assert!(!is_png(b"\xff\xd8\xff\xe0"));
        assert!(!is_png(b""));
    }
}
