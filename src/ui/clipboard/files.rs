//! Finder 文件条目 → 本地路径：`public.file-url` / `NSFilenamesPboardType`
//! 的三种形态兜底，以及 file:// URL（含文件引用 URL、百分号转义）的还原。

use std::path::PathBuf;

#[cfg(target_os = "macos")]
use crate::ui::mac_ffi::{ns_data_bytes, ns_string, ns_string_to_rust};

/// 剪切板里指向的文件（Finder 里 Cmd+C 复制文件）。
///
/// 同一份信息可能有三种形态，逐级兜底：
/// 1. `public.file-url` 按字符串读；
/// 2. `public.file-url` 只能按原始数据读（`stringForType:` 返回 nil 时）；
/// 3. 老式 `NSFilenamesPboardType` 的 plist 数组。
#[cfg(target_os = "macos")]
pub(super) fn pasteboard_file_paths(pb: *mut objc2::runtime::AnyObject) -> Vec<PathBuf> {
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
pub(super) fn file_url_to_path(url: &str) -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
