//! LSP wire vocabulary: the pushed-diagnostics payload, request timeouts, the
//! `textDocument/definition` result normalization, and path <-> `file://` URI
//! mapping.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lsp_types::LocationLink;
use serde_json::{Value, json};

pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(15);

/// Diagnostics pushed by the server via `textDocument/publishDiagnostics`.
#[derive(Clone, Debug)]
pub struct PublishedDiagnostics {
    pub uri: String,
    pub diagnostics: Vec<lsp_types::Diagnostic>,
}

/// Normalize a textDocument/definition result (`Location`, `Location[]`,
/// `LocationLink[]` or null) into `Vec<LocationLink>`.
pub(super) fn location_links_from_value(value: Value) -> Vec<LocationLink> {
    match value {
        Value::Null => vec![],
        Value::Array(items) => items.iter().filter_map(link_from_value).collect(),
        value @ Value::Object(_) => link_from_value(&value).into_iter().collect(),
        _ => vec![],
    }
}

fn link_from_value(value: &Value) -> Option<LocationLink> {
    if value.get("targetUri").is_some() {
        return serde_json::from_value::<LocationLink>(value.clone()).ok();
    }
    // Plain Location -> synthesize a LocationLink.
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

/// Percent-encode a filesystem path into a `file://` URI (spaces in
/// `Application Support` and similar must be encoded for lsp `Uri`).
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

/// Parse a `file://` URI back to a filesystem path (best effort).
#[allow(dead_code)] // used by tests; kept for future cross-file definition jumps
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
