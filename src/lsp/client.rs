//! [`LspClient`]: a cloneable handle to the language server, plus the request
//! plumbing (initialize handshake, requests, notifications, frame writing).

use std::collections::HashMap;
use std::path::Path;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use lsp_types::{CodeAction, CompletionResponse, Hover, LocationLink};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::protocol::{
    INITIALIZE_TIMEOUT, PublishedDiagnostics, REQUEST_TIMEOUT, location_links_from_value,
    path_to_uri,
};
use super::server::spawn_process;

/// Shared state of one language-server process. Cloned as an `Arc`; every field
/// is individually synchronized so no outer lock is needed.
pub(super) struct Inner {
    pub(super) next_id: AtomicI64,
    pub(super) writer_tx: Sender<String>,
    pub(super) pending: Mutex<HashMap<i64, Sender<Result<Value, String>>>>,
    pub(super) diagnostics_tx: Sender<PublishedDiagnostics>,
    pub(super) alive: AtomicBool,
    pub(super) child: Mutex<Option<Child>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A cloneable handle to the language server.
#[derive(Clone)]
pub struct LspClient {
    inner: Arc<Inner>,
    diagnostics_rx: Receiver<PublishedDiagnostics>,
}

impl LspClient {
    /// Spawn `typescript-language-server --stdio` rooted at `scripts_dir` and
    /// run the initialize handshake. Returns `Err` with the reason when the
    /// binary is not installed or the handshake fails/times out — the caller
    /// degrades gracefully to a syntax-highlighting-only editor.
    pub fn spawn(scripts_dir: &Path) -> Result<Self, String> {
        let (inner, diagnostics_rx, server) = spawn_process(scripts_dir)?;
        let client = Self {
            inner,
            diagnostics_rx,
        };
        // The server resolves the TypeScript compiler relative to the
        // workspace, which usually has no node_modules. Resolve the server's
        // real location (through version-manager symlinks) and point it at
        // the sibling `typescript` package it was installed with.
        let server_real = std::fs::canonicalize(&server).unwrap_or_else(|_| server.clone());
        let tsserver = server_real.ancestors().find_map(|dir| {
            let tsserver = dir.join("node_modules/typescript/lib/tsserver.js");
            tsserver.is_file().then_some(tsserver)
        });
        client.initialize(scripts_dir, tsserver.as_deref())?;
        eprintln!("LSP: ready ({})", server.display());
        Ok(client)
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// Drain all diagnostics pushed since the last call.
    pub fn take_diagnostics(&self) -> Vec<PublishedDiagnostics> {
        let mut out = Vec::new();
        while let Ok(d) = self.diagnostics_rx.try_recv() {
            out.push(d);
        }
        out
    }

    fn initialize(&self, root: &Path, tsserver_path: Option<&Path>) -> Result<(), String> {
        let mut initialization_options = json!({});
        if let Some(tsserver) = tsserver_path {
            initialization_options["tsserver"] = json!({ "path": tsserver });
        }
        // Client capabilities matter: `textDocument.publishDiagnostics` is
        // what makes typescript-language-server forward tsserver diagnostics
        // to us at all (features.diagnosticsSupport in its source).
        let params = json!({
            "processId": std::process::id(),
            "rootUri": path_to_uri(root),
            "capabilities": {
                "workspace": {
                    "applyEdit": true,
                    "workspaceEdit": { "documentChanges": false },
                    "didChangeConfiguration": { "dynamicRegistration": false },
                    "configuration": true,
                },
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "didSave": true,
                    },
                    "publishDiagnostics": {
                        "relatedInformation": true,
                        "versionSupport": false,
                    },
                    "hover": {
                        "contentFormat": ["markdown", "plaintext"],
                    },
                    "completion": {
                        "completionItem": {
                            "snippetSupport": false,
                            "commitCharactersSupport": true,
                            "documentationFormat": ["markdown", "plaintext"],
                            "labelDetailsSupport": false,
                        },
                    },
                    "definition": {
                        "linkSupport": true,
                    },
                    "codeAction": {
                        "dynamicRegistration": false,
                        "isPreferredSupport": true,
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": ["", "quickfix", "refactor", "source"],
                            },
                        },
                    },
                },
            },
            "initializationOptions": initialization_options,
        });
        self.request_with_timeout("initialize", params, INITIALIZE_TIMEOUT)
            .map(|_| {
                self.notify("initialized", json!({}));
            })
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT)
    }

    fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if !self.is_alive() {
            return Err("LSP server is not running".into());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = unbounded();
        self.inner.pending.lock().insert(id, tx);

        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.send_frame(&msg.to_string()) {
            self.inner.pending.lock().remove(&id);
            return Err(format!("LSP write failed: {e}"));
        }

        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                self.inner.pending.lock().remove(&id);
                Err("LSP request timed out".into())
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = self.send_frame(&msg.to_string());
    }

    fn send_frame(&self, body: &str) -> std::io::Result<()> {
        // `body.len()` is the byte length, which is what Content-Length needs.
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.inner
            .writer_tx
            .send(frame)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    }

    // ---- document sync ----

    pub fn did_open(&self, uri: &str, language_id: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 0,
                    "text": text,
                }
            }),
        );
    }

    pub fn did_change(&self, uri: &str, version: i32, text: &str) {
        // Full-document sync keeps the client trivial.
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}],
            }),
        );
    }

    pub fn did_close(&self, uri: &str) {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        );
    }

    // ---- typed requests ----

    pub fn completion(
        &self,
        uri: &str,
        position: lsp_types::Position,
        context: Option<lsp_types::CompletionContext>,
    ) -> Result<CompletionResponse, String> {
        let value = self.request(
            "textDocument/completion",
            json!({"textDocument": {"uri": uri}, "position": position, "context": context}),
        )?;
        match value {
            Value::Null => Ok(CompletionResponse::Array(vec![])),
            value => serde_json::from_value(value).map_err(|e| e.to_string()),
        }
    }

    pub fn hover(&self, uri: &str, position: lsp_types::Position) -> Result<Option<Hover>, String> {
        let value = self.request(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": position}),
        )?;
        serde_json::from_value::<Option<Hover>>(value).map_err(|e| e.to_string())
    }

    pub fn definition(
        &self,
        uri: &str,
        position: lsp_types::Position,
    ) -> Result<Vec<LocationLink>, String> {
        let value = self.request(
            "textDocument/definition",
            json!({"textDocument": {"uri": uri}, "position": position}),
        )?;
        Ok(location_links_from_value(value))
    }

    pub fn code_action(
        &self,
        uri: &str,
        start: lsp_types::Position,
        end: lsp_types::Position,
    ) -> Result<Vec<CodeAction>, String> {
        let value = self.request(
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": uri},
                "range": {"start": start, "end": end},
                "context": {"diagnostics": []},
            }),
        )?;
        match value {
            Value::Null => Ok(vec![]),
            Value::Array(items) => Ok(items
                .into_iter()
                .filter_map(|item| serde_json::from_value::<CodeAction>(item).ok())
                .collect()),
            value => Ok(serde_json::from_value::<CodeAction>(value)
                .ok()
                .into_iter()
                .collect()),
        }
    }

    pub fn execute_command(
        &self,
        command: &str,
        arguments: Option<Vec<Value>>,
    ) -> Result<Value, String> {
        self.request(
            "workspace/executeCommand",
            json!({"command": command, "arguments": arguments.unwrap_or_default()}),
        )
    }
}
