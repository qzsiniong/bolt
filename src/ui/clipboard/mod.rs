//! 粘贴内容模型 + NSPasteboard 读取与识别：Launcher 的 Cmd+V 内容识别链。
//!
//! [`PasteContent`] 是宿主公共类型：本模块负责从 NSPasteboard 读出它；
//! Launcher 用它做 token 过滤；各模块在 `LauncherItem.paste` 上声明自己能
//! 处理哪些变体。新增内容类型 = 这里加变体 + `classify_text`/读取链加一步
//! + 目标模块声明 handler。
//!
//! 读取分两层：[`peek_paste`] 只识别不读取（Cmd+V 时出 token、过滤列表），
//! [`read_paste_content`] 确认后才读完整内容（图片惰性到这一步）。
//! pin 模块经由 [`crate::modules::pin::clipboard::read_clipboard`] 复用
//! [`read_paste_content`]，再转成 pin 自己的展示类型。

use std::path::{Path, PathBuf};

// 文件形态的读取只有 macOS 用；其余平台保留代码（file:// URL 归一化是纯逻辑，
// 有测试盯着），但不参与编译期的死代码检查。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod files;
mod html;
mod image;

#[cfg(target_os = "macos")]
use files::pasteboard_file_paths;
use html::html_to_text;
use image::{image_dimensions, image_file_format, read_image_file};

use bolt_console::ConsoleChannel;

/// 从剪贴板识别出的内容类型（判定互斥，按优先级只有一个）。
#[derive(Clone, Debug)]
pub(crate) enum PasteContent {
    /// 位图：剪贴板/文件里的**原始格式**字节（PNG/TIFF/JPEG/GIF，gpui 原生
    /// 支持解码）。读取时不做任何转码——转码慢且无必要；另存、回写剪贴板
    /// 也保持原格式。
    Image {
        bytes: Vec<u8>,
        format: gpui_kit::ImageFormat,
        width: u32,
        height: u32,
    },
    /// 指向本地文件的路径（Finder 复制文件、或文本本身是存在的文件路径）。
    FilePath(PathBuf),
    /// 能被完整解析成 JSON 值的文本。
    Json(String),
    /// 单行 http(s) URL。
    Url(String),
    /// 其余纯文本。
    Text(String),
}

impl PasteContent {
    /// token renderer 用的图标 key（渲染端按它映射 Lucide 图标，
    /// 同时作为 `InlineToken` 的 id——其语义就是"引用的资源名"）。
    pub(crate) fn icon_key(&self) -> &'static str {
        match self {
            PasteContent::Image { .. } => "image",
            PasteContent::FilePath(_) => "file",
            PasteContent::Json(_) => "json",
            PasteContent::Url(_) => "link",
            PasteContent::Text(_) => "text",
        }
    }
}

/// 识别阶段的粘贴内容：图片**只识别类型不读字节**（整读 + 转 PNG 慢，
/// 不能卡 Cmd+V 的 UI），其余内容读取便宜（字符串 / stat / 文件头），
/// 识别时就读好。确认（回车）时才把惰性图片读成真内容。
pub(crate) enum PendingPaste {
    /// 图片：字节待读。来源是剪贴板里的图片**文件**时带路径（token 显示
    /// 文件名）；直接复制的位图（截图等）没有文件名。
    Image(Option<PathBuf>),
    /// 其余内容：已读出完整内容。
    Ready(PasteContent),
}

impl PendingPaste {
    /// token 徽章的显示文本：文件显示文件名，JSON/链接/文本显示前几个字符，
    /// 图片没有内容可预览，保持类型名。识别阶段这些都是已知的（图片惰性
    /// 不读字节，也只需要类型名）。
    pub(crate) fn token_text(&self) -> String {
        /// 预览最多保留的字符数（超出截断加省略号）。
        const MAX_CHARS: usize = 12;

        match self {
            PendingPaste::Image(path) => path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "图片".to_string()),
            PendingPaste::Ready(PasteContent::FilePath(path)) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "文件".to_string()),
            PendingPaste::Ready(content) => {
                // Json/Url/Text 的载体都是 String；预览折叠空白（多行 JSON
                // 里的换行会撑坏单行输入框）再截断。
                let text = match content {
                    PasteContent::Json(text)
                    | PasteContent::Url(text)
                    | PasteContent::Text(text) => text,
                    PasteContent::FilePath(_) | PasteContent::Image { .. } => {
                        unreachable!("上面分支已处理")
                    }
                };
                let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if flat.chars().count() <= MAX_CHARS {
                    flat
                } else {
                    flat.chars()
                        .take(MAX_CHARS)
                        .chain(std::iter::once('…'))
                        .collect()
                }
            }
        }
    }

    /// token renderer 用的图标 key（同 [`PasteContent::icon_key`]）。
    pub(crate) fn icon_key(&self) -> &'static str {
        match self {
            PendingPaste::Image(_) => "image",
            PendingPaste::Ready(content) => content.icon_key(),
        }
    }
}

/// 把一段纯文本分类成更具体的内容类型：URL → JSON → 本地文件路径 → 纯文本。
///
/// 判定尽量保守，宁可归成 Text 也不误判：
/// - URL 要求整段无空白（多行笔记即使以 http 开头也是 Text）；
/// - JSON 要求以 `{`/`[` 开头且能完整解析；
/// - 路径要求 `/` 或 `~` 开头且**真实存在**（是文件）。
pub(crate) fn classify_text(text: String) -> PasteContent {
    let trimmed = text.trim();
    if !trimmed.is_empty()
        && !trimmed.chars().any(char::is_whitespace)
        && (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
    {
        return PasteContent::Url(text);
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return PasteContent::Json(text);
    }
    if trimmed.starts_with('/') || trimmed.starts_with("~/") {
        let expanded = expand_home(trimmed);
        if Path::new(&expanded).is_file() {
            return PasteContent::FilePath(PathBuf::from(expanded));
        }
    }
    PasteContent::Text(text)
}

/// 展开 `~` 前缀为用户主目录（无主目录时原样返回）。
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

// ── NSPasteboard 读取（完整内容） ──────────────────────────────────────────

/// macOS 上直接发原始 Objective-C 消息（objc2 0.6 的 `msg_send!`）：
/// `[NSPasteboard generalPasteboard]` 拿共享剪切板，按优先级尝试文件形态
/// （`public.file-url`）、图片（`public.png` / `public.tiff` / `public.jpeg` /
/// `public.gif`）、文本（`public.html` / `public.utf8-plain-text`）。不用
/// `objc2-app-kit` / `objc2-foundation` 是因为它们目前依赖 objc2 0.5，会与
/// 本项目的 objc2 0.6 同时进入依赖图。图片字节再喂给 `image` 拿尺寸，窗口
/// 初始 bounds 用得到。
#[cfg(target_os = "macos")]
pub(crate) fn read_paste_content(console: &ConsoleChannel) -> Option<PasteContent> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use crate::ui::mac_ffi::{ns_data_bytes, ns_string, ns_string_to_rust};

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
            Ok((format, w, h, bytes)) => {
                console.info(format!("剪切板来源文件：{}（{w}×{h}）", path.display()));
                return Some(PasteContent::Image {
                    bytes,
                    format,
                    width: w,
                    height: h,
                });
            }
            Err(e) => console.warn(format!("剪切板是文件 {}：{e}", path.display())),
        }
        // 文件形态下不要落到文本分支：Finder 同时把文件名（如 `1995.jpg`）
        // 写进 plain-text 类型，pin 出来会是一个只有文件名的文本便签。
        // 不是可解码图片的文件按「文件路径」交给能处理文件的 handler（scripts）。
        return Some(PasteContent::FilePath(path.clone()));
    }

    for (uti, format) in [
        ("public.png", gpui_kit::ImageFormat::Png),
        ("public.tiff", gpui_kit::ImageFormat::Tiff),
        ("public.jpeg", gpui_kit::ImageFormat::Jpeg),
        ("public.gif", gpui_kit::ImageFormat::Gif),
    ] {
        let uti_obj = ns_string(uti);
        if uti_obj.is_null() {
            continue;
        }
        let data: *mut AnyObject = unsafe { msg_send![pb, dataForType: uti_obj] };
        let Some(bytes) = ns_data_bytes(data) else {
            continue;
        };

        // 不转码：按 UTI 定格式，原始字节直接给渲染端（gpui 原生解码这四种
        // 格式）。TIFF→PNG 的整读+重编码是粘贴卡顿的主因，一个字节都不碰。
        if let Some((w, h)) = image_dimensions(&bytes) {
            console.info(format!("剪切板来源位图：{uti}（{w}×{h}）"));
            return Some(PasteContent::Image {
                bytes: bytes.to_vec(),
                format,
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
                    "剪切板来源 HTML（已转纯文本，{} 字）",
                    text.chars().count()
                ));
                return Some(classify_text(text));
            }
        }
    }

    let utf8_key = ns_string("public.utf8-plain-text");
    if !utf8_key.is_null() {
        let nsstr: *mut AnyObject = unsafe { msg_send![pb, stringForType: utf8_key] };
        if let Some(text) = ns_string_to_rust(nsstr)
            && !text.trim().is_empty()
        {
            return Some(classify_text(text));
        }
    }

    None
}

/// Windows：走 `arboard` 读纯文本（HTML 富文本与文件形态暂未接，见下）。
///
/// 位图暂不支持——`arboard` 给的是裸 RGBA，要变成 gpui 能解码的通道得自己编码
/// PNG；这一步留到平台适配的第二阶段，当前只保证文本粘贴链路完整。
#[cfg(target_os = "windows")]
pub(crate) fn read_paste_content(_console: &ConsoleChannel) -> Option<PasteContent> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let text = clipboard.get_text().ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(classify_text(text))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn read_paste_content(_console: &ConsoleChannel) -> Option<PasteContent> {
    None
}

// ── NSPasteboard 识别（只看类型，不读字节） ────────────────────────────────

/// 只识别不读取：Cmd+V 时判定剪贴板里**是什么**（用于出类型 token 和过滤
/// 列表），位图不读字节、图片文件不整读——那些慢活留给确认时的
/// [`read_paste_content`]。分支顺序与 [`read_paste_content`] 一致，保证
/// peek 的类型和确认时读出的内容对得上。
#[cfg(target_os = "macos")]
pub(crate) fn peek_paste(_console: &ConsoleChannel) -> Option<PendingPaste> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use crate::ui::mac_ffi::{ns_data_bytes, ns_string, ns_string_to_rust};

    let pb: *mut AnyObject = unsafe {
        let cls = AnyClass::get(c"NSPasteboard")?;
        msg_send![cls, generalPasteboard]
    };
    if pb.is_null() {
        return None;
    }

    let file_paths = pasteboard_file_paths(pb);
    if let Some(path) = file_paths.first() {
        // 文件形态：只看文件头 magic 判断是不是图片（整读 + 转码延迟到
        // 确认时）；read_image_file 解不出的文件最终也按 FilePath 处理，
        // 这里直接归 FilePath，两边一致。
        return Some(match image_file_format(path) {
            Some(_) => PendingPaste::Image(Some(path.clone())),
            None => PendingPaste::Ready(PasteContent::FilePath(path.clone())),
        });
    }

    // 位图 UTI：只确认类型存在（dataForType 返回非空即有），不读不转。
    for uti in ["public.png", "public.tiff", "public.jpeg", "public.gif"] {
        let uti_obj = ns_string(uti);
        if uti_obj.is_null() {
            continue;
        }
        let data: *mut AnyObject = unsafe { msg_send![pb, dataForType: uti_obj] };
        if ns_data_bytes(data).is_some() {
            return Some(PendingPaste::Image(None));
        }
    }

    // 文本类读取便宜，与 read_paste_content 相同：HTML 优先于 plain-text。
    let html_key = ns_string("public.html");
    if !html_key.is_null() {
        let raw: *mut AnyObject = unsafe { msg_send![pb, stringForType: html_key] };
        if let Some(html) = ns_string_to_rust(raw) {
            let text = html_to_text(&html);
            if !text.trim().is_empty() {
                return Some(PendingPaste::Ready(classify_text(text)));
            }
        }
    }

    let utf8_key = ns_string("public.utf8-plain-text");
    if !utf8_key.is_null() {
        let nsstr: *mut AnyObject = unsafe { msg_send![pb, stringForType: utf8_key] };
        if let Some(text) = ns_string_to_rust(nsstr)
            && !text.trim().is_empty()
        {
            return Some(PendingPaste::Ready(classify_text(text)));
        }
    }

    None
}

/// Windows：与 [`read_paste_content`] 同一套判定（只读文本，不读位图）。
#[cfg(target_os = "windows")]
pub(crate) fn peek_paste(_console: &ConsoleChannel) -> Option<PendingPaste> {
    read_paste_content(_console).map(PendingPaste::Ready)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn peek_paste(_console: &ConsoleChannel) -> Option<PendingPaste> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(text: &str) -> PasteContent {
        classify_text(text.to_string())
    }

    #[test]
    fn classifies_text_variants() {
        assert!(matches!(classify("http://a.b/c"), PasteContent::Url(_)));
        assert!(matches!(classify("https://a.b"), PasteContent::Url(_)));
        // 多行文本即使以 URL 开头也不是链接。
        assert!(matches!(
            classify("https://a.b\n第二行"),
            PasteContent::Text(_)
        ));
        assert!(matches!(classify(r#"{"a": 1}"#), PasteContent::Json(_)));
        assert!(matches!(classify("[1, 2]"), PasteContent::Json(_)));
        // 解析失败 / 半截 JSON 是纯文本。
        assert!(matches!(classify("{oops"), PasteContent::Text(_)));
        assert!(matches!(classify("{\"a\": 1} 尾巴"), PasteContent::Text(_)));
        // 不存在的路径是纯文本，真实存在的文件是 FilePath。
        assert!(matches!(
            classify("/nonexistent/path.py"),
            PasteContent::Text(_)
        ));
        let tmp = crate::test_support::TempDir::new("paste-classify");
        let script = tmp.0.join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert!(matches!(
            classify(script.to_string_lossy().as_ref()),
            PasteContent::FilePath(_)
        ));
        assert!(matches!(classify("hello world"), PasteContent::Text(_)));
    }

    #[test]
    fn token_text_previews_content() {
        use std::path::PathBuf;
        // 图片：截图位图无文件名 → 类型名；图片文件 → 文件名。
        assert_eq!(PendingPaste::Image(None).token_text(), "图片");
        assert_eq!(
            PendingPaste::Image(Some(PathBuf::from("/tmp/a/shot.png"))).token_text(),
            "shot.png"
        );
        // 文件：文件名（不是全路径）。
        assert_eq!(
            PendingPaste::Ready(PasteContent::FilePath(PathBuf::from("/tmp/a/b.json")))
                .token_text(),
            "b.json"
        );
        // 长文本：截断 + 省略号；多行折叠成单行。
        let t = PendingPaste::Ready(PasteContent::Text(
            "你好，世界！这是一段比较长的文本内容".into(),
        ))
        .token_text();
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 13);
        assert_eq!(
            PendingPaste::Ready(PasteContent::Text("a\n  b".into())).token_text(),
            "a b"
        );
    }
}
