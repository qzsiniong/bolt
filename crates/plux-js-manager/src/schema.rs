//! 清单 schema 的极简校验器。
//!
//! `schemas/extension.schema.json` 是手写的，得有东西盯着它别与实际清单漂移：
//! 本模块只实现该 schema 用到的那几个关键字（`type`、`enum`、`const`、
//! `required`、`properties`、`additionalProperties`、`items`、`$ref`、`oneOf`、
//! `not`），返回问题列表而不是 bool，失败时能直接看出哪一层对不上。
//!
//! 刻意**不是**完整实现：`pattern`、`minLength` 之类不查（那要引 regex）。
//! 它服务于测试与内部工具，不参与插件加载路径。

use serde_json::Value;

/// 逐层比对，返回全部问题；空 Vec = 通过。
pub fn problems(schema: &Value, value: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    check(schema, schema, value, "$", &mut problems);
    problems
}

fn check(root: &Value, node: &Value, value: &Value, path: &str, out: &mut Vec<String>) {
    let Some(object) = node.as_object() else {
        return;
    };
    let node = match object.get("$ref").and_then(Value::as_str) {
        Some(reference) => root
            .pointer(reference.trim_start_matches('#'))
            .unwrap_or(node),
        None => node,
    };

    if let Some(kind) = node["type"].as_str() {
        let matches = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => true,
        };
        if !matches {
            out.push(format!("{path}: 期望 {kind}"));
            return;
        }
    }

    if let Some(options) = node["enum"].as_array()
        && !options.contains(value)
    {
        out.push(format!("{path}: {value} 不在 enum {options:?} 里"));
    }
    if let Some(expected) = node.get("const")
        && expected != value
    {
        out.push(format!("{path}: 期望常量 {expected}，实际 {value}"));
    }

    if let Some(value) = value.as_object() {
        for required in node["required"].as_array().into_iter().flatten() {
            let key = required.as_str().unwrap_or_default();
            if !value.contains_key(key) {
                out.push(format!("{path}: 缺少必填字段 `{key}`"));
            }
        }
        if let Some(properties) = node["properties"].as_object() {
            let strict = node["additionalProperties"] == Value::Bool(false);
            for (key, child) in value {
                match properties.get(key) {
                    Some(schema) => check(root, schema, child, &format!("{path}.{key}"), out),
                    None if strict => out.push(format!("{path}: 多出未声明的字段 `{key}`")),
                    None => {}
                }
            }
        }
    }

    if let (Some(array), Some(items)) = (value.as_array(), node["items"].as_object()) {
        let items = Value::Object(items.clone());
        for (index, child) in array.iter().enumerate() {
            check(root, &items, child, &format!("{path}[{index}]"), out);
        }
    }

    if let Some(branches) = node["oneOf"].as_array() {
        let satisfied = branches
            .iter()
            .filter(|branch| {
                let mut nested = Vec::new();
                check(root, branch, value, path, &mut nested);
                nested.is_empty()
            })
            .count();
        if satisfied != 1 {
            out.push(format!("{path}: oneOf 命中了 {satisfied} 个分支（应为 1）"));
        }
    }

    if let Some(branch) = node.get("not") {
        let mut nested = Vec::new();
        check(root, branch, value, path, &mut nested);
        if nested.is_empty() {
            out.push(format!("{path}: 命中了不该命中的 not 分支"));
        }
    }
}
