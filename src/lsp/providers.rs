//! 把 [`LspClient`] 接进 gpui-kit 编辑器的 LSP provider trait。
//! 单个 [`LspBridge`] 同时实现补全、悬停、跳转与代码操作；编辑器当前打开的
//! 文档通过共享的 [`DocHandle`] 追踪，保证 provider 总是打到正确的 URI 上。

use std::ops::Range;
use std::str::FromStr;
use std::sync::Arc;

use gpui_kit::component::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider, EditorState, HoverProvider, Rope,
    RopeExt,
};
use gpui_kit::{App, AppContext, Entity, SharedString, Task, Window};
use lsp_types::{CodeAction, CompletionContext, CompletionResponse, Hover, Position};
use parking_lot::Mutex;

use super::client::LspClient;

/// 编辑器里当前载入的文档，面板负责写、provider 负责读。
#[derive(Clone, Debug)]
pub struct DocHandle {
    /// 文档的 `file://` URI。
    pub uri: String,
    /// 文档版本号，`didChange` 时递增。
    pub version: i32,
}

/// 一个桥实现全部四个 gpui LSP provider trait。
#[derive(Clone)]
pub struct LspBridge {
    client: Option<Arc<LspClient>>,
    doc: Arc<Mutex<Option<DocHandle>>>,
}

impl LspBridge {
    /// `client` 为 `None` 时桥仍然可用，只是所有请求都返回空结果。
    pub fn new(client: Option<Arc<LspClient>>, doc: Arc<Mutex<Option<DocHandle>>>) -> Self {
        Self { client, doc }
    }

    /// 取出当前文档、客户端，并把字节偏移换算成 LSP 位置。LSP 不可用或没有
    /// 打开文档时返回 `None`。
    fn active(&self, text: &Rope, offset: usize) -> Option<(Arc<LspClient>, String, Position)> {
        let client = self.client.as_ref()?;
        let doc = self.doc.lock();
        let doc = doc.as_ref()?;
        Some((
            client.clone(),
            doc.uri.clone(),
            text.offset_to_position(offset),
        ))
    }
}

impl CompletionProvider for LspBridge {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let Some((client, uri, position)) = self.active(text, offset) else {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        };
        // tsserver 返回的是整个作用域/成员列表，不做过滤——过滤是这一层的
        // 责任（编辑器只显示我们返回的东西）。按光标前的单词过滤，但 `.` 这类
        // 非单词触发符之后不过滤（成员列表本身都是合法候选）。
        let query = word_before(text, offset);
        let word_start = offset.saturating_sub(query.len());
        let word_start_pos = text.offset_to_position(word_start);
        let cursor_pos = text.offset_to_position(offset);
        let member_access = matches!(
            trigger.trigger_character.as_deref().and_then(|s| s.chars().last()),
            Some(c) if !c.is_alphanumeric() && c != '_' && c != '$'
        );
        cx.background_spawn(async move {
            let response = client
                .completion(&uri, position, Some(trigger))
                .map_err(anyhow::Error::msg)?;
            let mut items = match response {
                CompletionResponse::Array(items) => items,
                CompletionResponse::List(list) => list.items,
            };
            if !member_access && !query.is_empty() {
                let q = query.to_lowercase();
                items.retain(|item| {
                    let candidate = item.filter_text.as_deref().unwrap_or(item.label.as_str());
                    candidate.to_lowercase().starts_with(&q)
                });
            }
            // tsserver 的补全项不带 text_edit，编辑器会回退成替换
            // `trigger_start_offset..cursor`——成员访问时那段从 `.` 开始，会把
            // `.` 一起吃掉。所以给每一项补一个只覆盖待补全单词的 text_edit。
            let edit_range = lsp_types::Range::new(word_start_pos, cursor_pos);
            for item in &mut items {
                if item.text_edit.is_none() {
                    let new_text = item
                        .insert_text
                        .clone()
                        .unwrap_or_else(|| item.label.clone());
                    item.text_edit =
                        Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                            range: edit_range,
                            new_text,
                        }));
                }
            }
            items.truncate(200);
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .map(|c| c.is_alphanumeric() || "_$.\"'".contains(c))
            .unwrap_or(false)
    }
}

/// `offset` 之前紧邻的标识符（没有则返回空串）。判定规则与编辑器自带的单词
/// 识别一致（`is_alphanumeric` 加 `_`），再为 JS 补上 `$`。
fn word_before(text: &Rope, offset: usize) -> String {
    let word_len: usize = text
        .chars_at(offset)
        .reversed()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '$')
        .map(char::len_utf8)
        .sum();
    let start = offset.saturating_sub(word_len);
    text.slice(start..offset).to_string()
}

impl HoverProvider for LspBridge {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<Hover>>> {
        let Some((client, uri, position)) = self.active(text, offset) else {
            return Task::ready(Ok(None));
        };
        cx.background_spawn(async move { client.hover(&uri, position).map_err(anyhow::Error::msg) })
    }
}

impl DefinitionProvider for LspBridge {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        let Some((client, uri, position)) = self.active(text, offset) else {
            return Task::ready(Ok(vec![]));
        };
        cx.background_spawn(async move {
            client
                .definition(&uri, position)
                .map_err(anyhow::Error::msg)
        })
    }
}

impl CodeActionProvider for LspBridge {
    fn id(&self) -> SharedString {
        "bolt-lsp".into()
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<CodeAction>>> {
        let text = state.read(cx).text().clone();
        let Some((client, uri, start)) = self.active(&text, range.start) else {
            return Task::ready(Ok(vec![]));
        };
        let end = text.offset_to_position(range.end);
        cx.background_spawn(async move {
            client
                .code_action(&uri, start, end)
                .map_err(anyhow::Error::msg)
        })
    }

    fn perform_code_action(
        &self,
        state: Entity<EditorState>,
        action: CodeAction,
        _push_to_history: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        // 当前文档的 workspace edit 得在 UI 线程上应用。
        if let Some(edit) = action.edit
            && let Some(changes) = edit.changes
        {
            let doc_uri = self.doc.lock().as_ref().map(|d| d.uri.clone());
            if let Some(uri_str) = doc_uri
                && let Ok(uri) = lsp_types::Uri::from_str(&uri_str)
                && let Some(text_edits) = changes.get(&uri)
            {
                let text_edits = text_edits.clone();
                state.update(cx, |state, cx| {
                    state.apply_lsp_edits(&text_edits, window, cx);
                });
            }
        }

        // 只有 command 的动作（例如 Organize Imports）交回服务器执行。
        if let Some(command) = action.command {
            let Some(client) = self.client.clone() else {
                return Task::ready(Ok(()));
            };
            cx.background_spawn(async move {
                client
                    .execute_command(&command.command, command.arguments)
                    .map(|_| ())
                    .map_err(anyhow::Error::msg)
            })
        } else {
            Task::ready(Ok(()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::word_before;
    use gpui_kit::component::input::Rope;

    #[test]
    fn word_before_extracts_identifier_prefix() {
        let rope = Rope::from("console.log(1);\nconsole");
        let end = rope.len();
        assert_eq!(word_before(&rope, end), "console");

        // 点号之后没有单词前缀（成员访问）。
        let dot = "console.".len();
        assert_eq!(word_before(&rope, dot), "");

        // JS 标识符可以包含 `$`。
        let rope = Rope::from("const $el = 1;\n$el");
        let end = rope.len();
        assert_eq!(word_before(&rope, end), "$el");

        // 空文档 / 行首。
        let rope = Rope::from("");
        assert_eq!(word_before(&rope, 0), "");
    }
}
