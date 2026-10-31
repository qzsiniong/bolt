//! pin 便签的内容类型与读取入口。
//!
//! NSPasteboard 读取与内容识别（[`read_paste_content`] / `peek_paste`）在
//! `ui/clipboard.rs`——那是 Launcher Cmd+V 识别与 pin 共用的宿主公共层。
//! 本模块只做 pin 专属的一层转换：[`read_clipboard`] 读出宿主公共的
//! [`PasteContent`] 后转成 pin 能展示的 [`PinContent`]（非图片文件不 pin，
//! 保持 pin 原有语义），并给出便签的展示尺寸。

use std::path::PathBuf;

use gpui_kit::ImageFormat;

use crate::ui::clipboard::{PasteContent, read_paste_content};
use bolt_console::ConsoleChannel;

/// pin 便签展示的最大尺寸（逻辑像素）。
pub(crate) const MAX_PIN_WIDTH: f32 = 800.;
pub(crate) const MAX_PIN_HEIGHT: f32 = 600.;

/// 剪切板里读到的内容类型。
pub enum PinContent {
    Image {
        /// 原始格式字节（PNG/TIFF/JPEG/GIF）：读取时不转码，渲染端按
        /// `format` 解码（gpui 原生支持这四种）。
        bytes: Vec<u8>,
        format: ImageFormat,
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

/// pin 的入口：读出 [`PasteContent`] 后转成 pin 能展示的类型。非图片文件
/// 不 pin（保持 pin 原有语义）。
pub fn read_clipboard(console: &ConsoleChannel) -> Option<PinContent> {
    read_paste_content(console).and_then(PinContent::try_from_paste)
}

impl PinContent {
    /// [`PasteContent`] → pin 展示类型：图片原样；非图片文件不 pin（None）；
    /// 其余文本类当文本便签。
    pub(crate) fn try_from_paste(content: PasteContent) -> Option<PinContent> {
        match content {
            PasteContent::Image {
                bytes,
                format,
                width,
                height,
            } => Some(PinContent::Image {
                bytes,
                format,
                width,
                height,
            }),
            PasteContent::Json(t) | PasteContent::Url(t) | PasteContent::Text(t) => {
                Some(PinContent::Text(t))
            }
            // pin 是图片/文本便签；文件路径交给 scripts 等 handler。
            PasteContent::FilePath(_) => None,
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn read_clipboard(_console: &ConsoleChannel) -> Option<PinContent> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_size_scales_images_and_keeps_text_width() {
        // 小图不放大；大图按比例缩进 800×600 上限；文本固定 320 宽。
        let small = PinContent::Image {
            bytes: vec![],
            format: ImageFormat::Png,
            width: 400,
            height: 300,
        };
        assert_eq!(small.display_size(), (400., 300.));

        let large = PinContent::Image {
            bytes: vec![],
            format: ImageFormat::Tiff,
            width: 2400,
            height: 1200,
        };
        // scale = min(800/2400, 600/1200) = 1/3
        assert_eq!(large.display_size(), (800., 400.));

        let (w, h) = PinContent::Text("hello".to_string()).display_size();
        assert_eq!(w, 320.);
        assert!(h > 0.);
    }
}
