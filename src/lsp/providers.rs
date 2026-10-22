//! Bridges the [`LspClient`] into gpui-kit's Editor LSP provider traits.
//! A single [`LspBridge`] implements completion, hover, definition and code
//! actions; the editor's currently open document is tracked through a shared
//! [`DocHandle`] so providers always target the right URI.

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

/// The document currently loaded in the editor, shared between the panel
/// (which updates it) and the providers (which read it).
#[derive(Clone, Debug)]
pub struct DocHandle {
    pub uri: String,
    pub version: i32,
}

/// One bridge implementing all four gpui LSP provider traits.
#[derive(Clone)]
pub struct LspBridge {
    client: Option<Arc<LspClient>>,
    doc: Arc<Mutex<Option<DocHandle>>>,
}

impl LspBridge {
    pub fn new(client: Option<Arc<LspClient>>, doc: Arc<Mutex<Option<DocHandle>>>) -> Self {
        Self { client, doc }
    }

    /// Current document + LSP position for a byte offset, or `None` when the
    /// LSP is unavailable / no document is open.
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
        // tsserver returns the whole scope/member list unfiltered — client-side
        // filtering is the provider's job here (the editor shows whatever we
        // return). Filter by the word before the caret, except after a
        // non-word trigger like `.` (member access lists everything validly).
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
            // tsserver items carry no text_edit; the editor's fallback replaces
            // `trigger_start_offset..cursor` which starts at `.` for member
            // access, eating the dot. Give every item an explicit text_edit
            // covering only the word being completed.
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

/// The identifier immediately before `offset` ("" when none). Mirrors the
/// editor's own word detection (`is_alphanumeric` + `_`), plus `$` for JS.
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
        // Apply the workspace edit for the current document on the UI thread.
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

        // Delegate command-only actions (e.g. Organize Imports) to the server.
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

        // After a dot there is no word prefix (member access).
        let dot = "console.".len();
        assert_eq!(word_before(&rope, dot), "");

        // JS identifiers may contain `$`.
        let rope = Rope::from("const $el = 1;\n$el");
        let end = rope.len();
        assert_eq!(word_before(&rope, end), "$el");

        // Empty document / start of line.
        let rope = Rope::from("");
        assert_eq!(word_before(&rope, 0), "");
    }
}
