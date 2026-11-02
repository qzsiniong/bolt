//! JS 值与宿主字符串之间的转换。
//!
//! 命令的返回值统一走「JSON 字符串」这一条通道：命令的返回值本来就是给
//! Console 看的一段文本，没必要引入更复杂的编解码。

use rquickjs::{CaughtError, Ctx, Value};

/// 把 JS 返回值转成宿主可读的字符串。
///
/// `undefined` / `null` → `None`；字符串原样返回；其余走 `JSON.stringify`
/// （函数与 symbol 会得到 `None`，与 JS 语义一致）。
pub fn value_to_json<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> Result<Option<String>, rquickjs::Error> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    if let Some(text) = value.as_string() {
        return Ok(Some(text.to_string()?));
    }
    match ctx.json_stringify(value.clone())? {
        Some(text) => Ok(Some(text.to_string()?)),
        None => Ok(None),
    }
}

/// 把一段 JSON 文本解析成 JS 值；空串当作 `undefined`。
pub fn json_to_value<'js>(ctx: &Ctx<'js>, json: &str) -> Result<Value<'js>, rquickjs::Error> {
    if json.trim().is_empty() {
        return Ok(Value::new_undefined(ctx.clone()));
    }
    ctx.json_parse(json)
}

/// 异常 → 可读文本：异常消息 + 堆栈（堆栈里已经带消息时不重复）。
pub fn format_caught(err: &CaughtError) -> String {
    match err {
        CaughtError::Exception(exception) => {
            let message = exception.to_string();
            match exception.stack() {
                Some(stack) if !stack.trim().is_empty() => {
                    if stack.contains(message.trim()) {
                        stack
                    } else {
                        format!("{message}\n{stack}")
                    }
                }
                _ => message,
            }
        }
        CaughtError::Value(value) => format!("{value:?}"),
        CaughtError::Error(error) => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rquickjs::{Context, Runtime};

    fn with_ctx(f: impl for<'js> FnOnce(&Ctx<'js>)) {
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        ctx.with(|ctx| f(&ctx));
    }

    #[test]
    fn undefined_and_null_become_none() {
        with_ctx(|ctx| {
            assert_eq!(
                value_to_json(ctx, &Value::new_undefined(ctx.clone())).unwrap(),
                None
            );
            assert_eq!(
                value_to_json(ctx, &Value::new_null(ctx.clone())).unwrap(),
                None
            );
        });
    }

    #[test]
    fn strings_pass_through_and_objects_are_serialized() {
        with_ctx(|ctx| {
            let text: Value = ctx.eval("'hi'").unwrap();
            assert_eq!(value_to_json(ctx, &text).unwrap().as_deref(), Some("hi"));

            let object: Value = ctx.eval("({ a: 1, b: [2, 3] })").unwrap();
            assert_eq!(
                value_to_json(ctx, &object).unwrap().as_deref(),
                Some(r#"{"a":1,"b":[2,3]}"#)
            );
        });
    }

    #[test]
    fn json_round_trips() {
        with_ctx(|ctx| {
            let value = json_to_value(ctx, r#"{"command":"hello"}"#).unwrap();
            assert!(value.is_object());
            assert!(json_to_value(ctx, "  ").unwrap().is_undefined());
        });
    }
}
