//! 严格 JSON 的格式化辅助。不容忍 JSONC / JSON5 / JS 字面量等——异种输入
//! 的多路兜底解析保留在 `extensions/json-editor/` 扩展里。

use serde_json::Value;

/// 把严格 JSON 字符串美化输出（2 空格缩进）。
pub fn format(text: &str) -> Result<String, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;
    serde_json::to_string_pretty(&v)
}

/// 把严格 JSON 字符串压缩输出（无空白）。
pub fn minify(text: &str) -> Result<String, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;
    serde_json::to_string(&v)
}

/// 产出输入的 JSON 字符串形式——等价于 JS 里的 `JSON.stringify`：加上
/// 外层引号，转义反斜杠 / 引号 / 控制字符。
pub fn escape(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}
