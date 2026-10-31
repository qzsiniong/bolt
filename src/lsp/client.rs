//! [`LspClient`]：语言服务器的可克隆句柄，以及请求管线（初始化握手、请求、
//! 通知、帧写入）。

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

/// 一个语言服务器进程的共享状态。以 `Arc` 克隆；每个字段各自同步，所以不需要
/// 外层大锁。
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

/// 语言服务器的可克隆句柄。
#[derive(Clone)]
pub struct LspClient {
    inner: Arc<Inner>,
    diagnostics_rx: Receiver<PublishedDiagnostics>,
}

impl LspClient {
    /// 以 `scripts_dir` 为根启动 `typescript-language-server --stdio`，并完成
    /// initialize 握手。可执行文件没装、或握手失败/超时时返回 `Err` 说明原因
    /// ——调用方据此优雅降级成「只有语法高亮」的编辑器。
    pub fn spawn(scripts_dir: &Path) -> Result<Self, String> {
        let (inner, diagnostics_rx, server) = spawn_process(scripts_dir)?;
        let client = Self {
            inner,
            diagnostics_rx,
        };
        // 服务器按工作区去找 TypeScript 编译器，而工作区通常没有
        // node_modules。所以先解析出服务器自身的真实路径（穿过版本管理器的
        // 软链），再指向与它一同安装的那个 `typescript` 包。
        let server_real = std::fs::canonicalize(&server).unwrap_or_else(|_| server.clone());
        let tsserver = server_real.ancestors().find_map(|dir| {
            let tsserver = dir.join("node_modules/typescript/lib/tsserver.js");
            tsserver.is_file().then_some(tsserver)
        });
        client.initialize(scripts_dir, tsserver.as_deref())?;
        // 就绪与否由 boot 侧统一经 ConsoleChannel 上报（stderr 打包后不可见）。
        Ok(client)
    }

    /// 服务器进程是否仍活着。
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// 取走自上次调用以来服务器推送的全部诊断。
    pub fn take_diagnostics(&self) -> Vec<PublishedDiagnostics> {
        let mut out = Vec::new();
        while let Ok(d) = self.diagnostics_rx.try_recv() {
            out.push(d);
        }
        out
    }

    /// 握手：发送 `initialize` 声明能力，成功后补一条 `initialized` 通知。
    fn initialize(&self, root: &Path, tsserver_path: Option<&Path>) -> Result<(), String> {
        let mut initialization_options = json!({});
        if let Some(tsserver) = tsserver_path {
            initialization_options["tsserver"] = json!({ "path": tsserver });
        }
        // 客户端能力声明很关键：正是 `textDocument.publishDiagnostics` 让
        // typescript-language-server 把 tsserver 诊断转发给我们（对应它源码
        // 里的 features.diagnosticsSupport）。
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
        // `body.len()` 是字节长度，正是 Content-Length 需要的值。
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.inner
            .writer_tx
            .send(frame)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    }

    // ---- 文档同步 ----

    /// 通知服务器打开了文档（初始版本号为 0）。
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

    /// 全量推送文档内容，`version` 由调用方递增。
    pub fn did_change(&self, uri: &str, version: i32, text: &str) {
        // 全文档同步，客户端逻辑因此可以保持极简。
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}],
            }),
        );
    }

    /// 通知服务器关闭文档。
    pub fn did_close(&self, uri: &str) {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        );
    }

    // ---- 带类型的请求 ----

    /// `textDocument/completion`。服务器返回 null 时当作空列表。
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

    /// `textDocument/hover`。
    pub fn hover(&self, uri: &str, position: lsp_types::Position) -> Result<Option<Hover>, String> {
        let value = self.request(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": position}),
        )?;
        serde_json::from_value::<Option<Hover>>(value).map_err(|e| e.to_string())
    }

    /// `textDocument/definition`，结果统一成 `LocationLink` 列表。
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

    /// `textDocument/codeAction`。这里不带诊断上下文，只取光标区间的动作。
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

    /// `workspace/executeCommand`，用于只有命令、没有编辑内容的动作。
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
