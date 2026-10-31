//! LSP 协议词汇：推送式诊断的载荷、请求超时、`textDocument/definition`
//! 结果归一化，以及路径与 `file://` URI 的互转。

use std::path::{Path, PathBuf};
use std::time::Duration;

use lsp_types::LocationLink;
use serde_json::{Value, json};

/// 普通请求的超时上限：服务器不答就报错，别让编辑器一直等着。
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// 初始化另算：tsserver 首次启动要加载整个项目，给足时间。
pub(super) const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(15);

/// 服务器通过 `textDocument/publishDiagnostics` 推来的诊断。
#[derive(Clone, Debug)]
pub struct PublishedDiagnostics {
    /// 文档的 `file://` URI。
    pub uri: String,
    pub diagnostics: Vec<lsp_types::Diagnostic>,
}

/// 把 `textDocument/definition` 的结果（`Location`、`Location[]`、
/// `LocationLink[]` 或 null）统一成 `Vec<LocationLink>`。
pub(super) fn location_links_from_value(value: Value) -> Vec<LocationLink> {
    match value {
        Value::Null => vec![],
        Value::Array(items) => items.iter().filter_map(link_from_value).collect(),
        value @ Value::Object(_) => link_from_value(&value).into_iter().collect(),
        _ => vec![],
    }
}

/// 把单个结果项转成 `LocationLink`：本身就是 link 的直接反序列化，普通
/// `Location` 则用它的 uri + range 现场合成一个。
fn link_from_value(value: &Value) -> Option<LocationLink> {
    if value.get("targetUri").is_some() {
        return serde_json::from_value::<LocationLink>(value.clone()).ok();
    }
    // 普通 Location → 补成一个 LocationLink。
    if value.get("uri").is_some() {
        let range = value.get("range").cloned()?;
        return serde_json::from_value::<LocationLink>(json!({
            "targetUri": value["uri"],
            "targetRange": range,
            "targetSelectionRange": range,
        }))
        .ok();
    }
    None
}

/// 把文件系统路径百分号编码成 `file://` URI（`Application Support` 这类路径里
/// 的空格必须编码，否则 lsp 的 `Uri` 解析不了）。
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// 把 `file://` URI 解析回文件系统路径（尽力而为）。
#[allow(dead_code)] // 目前只有测试用；留给后续的跨文件跳转
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded: String = rest
        .split(['/', '?'])
        .map(|seg| {
            let mut bytes = Vec::new();
            let mut chars = seg.bytes();
            while let Some(b) = chars.next() {
                if b == b'%' {
                    let hi = chars.next()?;
                    let lo = chars.next()?;
                    let hex = [hi, lo];
                    let hex = std::str::from_utf8(&hex).ok()?;
                    bytes.push(u8::from_str_radix(hex, 16).ok()?);
                } else {
                    bytes.push(b);
                }
            }
            String::from_utf8(bytes).ok()
        })
        .collect::<Option<Vec<_>>>()?
        .join("/");
    Some(PathBuf::from(decoded))
}
