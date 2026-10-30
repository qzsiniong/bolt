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

use crate::console_channel::ConsoleChannel;
#[cfg(target_os = "macos")]
use crate::ui::mac_ffi::{ns_data_bytes, ns_string, ns_string_to_rust};

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

#[cfg(not(target_os = "macos"))]
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

#[cfg(not(target_os = "macos"))]
pub(crate) fn peek_paste(_console: &ConsoleChannel) -> Option<PendingPaste> {
    None
}

// ── 识别链辅助 ─────────────────────────────────────────────────────────────

/// 只读文件头 16 字节按 magic 识别图片格式，不整读文件。
fn image_file_format(path: &Path) -> Option<gpui_kit::ImageFormat> {
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
fn detect_image_format(bytes: &[u8]) -> Option<gpui_kit::ImageFormat> {
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

/// 读一个本地图片文件 → (格式, 宽, 高, 原始字节)。**不转码**：gpui 原生
/// 解码 PNG/TIFF/JPEG/GIF，渲染与另存都用原格式；整读后只做 magic 识别
/// 和文件头尺寸解析，一个像素都不碰。
///
/// 返回具体的失败原因：文件形态下不能静默失败去回退图标位图，得让用户看到
/// 到底是没权限还是文件不是图片。
fn read_image_file(path: &Path) -> Result<(gpui_kit::ImageFormat, u32, u32, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取失败：{e}"))?;
    let format = detect_image_format(&bytes).ok_or_else(|| "不是可解码的图片".to_string())?;
    let (w, h) = image_dimensions(&bytes).ok_or_else(|| "图片头解析失败".to_string())?;
    Ok((format, w, h, bytes))
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

/// 只读图片头拿宽高（不做全图解码）：读取链跑在 UI 线程（Cmd+V 的
/// on_paste 回调里），全图解码一张 retina 截图也要几百毫秒。
fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
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
