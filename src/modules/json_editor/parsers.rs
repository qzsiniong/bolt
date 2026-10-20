//! Strict JSON format helpers. No tolerance for JSONC / JSON5 / JS literals
//! etc. — the `extensions/json-editor/` extension keeps the multi-fallback
//! parser for exotic inputs.

use serde_json::Value;

/// Pretty-print (2-space indent) a strict JSON string.
pub fn format(text: &str) -> Result<String, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;
    serde_json::to_string_pretty(&v)
}

/// Minify (no whitespace) a strict JSON string.
pub fn minify(text: &str) -> Result<String, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;
    serde_json::to_string(&v)
}

/// Produce the JSON string form of the input — equivalent to `JSON.stringify`
/// in JS: adds the surrounding quotes and escapes backslashes / quotes /
/// control characters.
pub fn escape(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}
