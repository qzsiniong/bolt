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

use crate::lsp::LspClient;

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
        cx.background_spawn(async move {
            client
                .completion(&uri, position, Some(trigger))
                .map_err(anyhow::Error::msg)
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
        "scripter-lsp".into()
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
        if let Some(edit) = action.edit {
            if let Some(changes) = edit.changes {
                let doc_uri = self.doc.lock().as_ref().map(|d| d.uri.clone());
                if let Some(uri_str) = doc_uri {
                    if let Ok(uri) = lsp_types::Uri::from_str(&uri_str) {
                        if let Some(text_edits) = changes.get(&uri) {
                            let text_edits = text_edits.clone();
                            state.update(cx, |state, cx| {
                                state.apply_lsp_edits(&text_edits, window, cx);
                            });
                        }
                    }
                }
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
