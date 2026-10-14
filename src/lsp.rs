//! Minimal LSP client speaking JSON-RPC over stdio with
//! `typescript-language-server`. Runs the server as a child process and
//! bridges requests/notifications through blocking threads, so callers
//! (GPUI background tasks) use plain channel waits.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use lsp_types::{CodeAction, CompletionResponse, Hover, LocationLink, PublishDiagnosticsParams};
use parking_lot::Mutex;
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(15);

/// Diagnostics pushed by the server via `textDocument/publishDiagnostics`.
#[derive(Clone, Debug)]
pub struct PublishedDiagnostics {
    pub uri: String,
    pub diagnostics: Vec<lsp_types::Diagnostic>,
}

struct Inner {
    next_id: AtomicI64,
    writer_tx: Sender<String>,
    pending: Mutex<HashMap<i64, Sender<Result<Value, String>>>>,
    diagnostics_tx: Sender<PublishedDiagnostics>,
    alive: AtomicBool,
    child: Mutex<Option<Child>>,
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
        let server = find_server_binary()?;
        let mut command = Command::new(&server);
        command
            .arg("--stdio")
            .current_dir(scripts_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // The server script's shebang (`#!/usr/bin/env node`) needs `node` on
        // PATH. When the app is launched from Finder the inherited PATH is
        // minimal, so prepend the directory the server was found in (fnm/nvm
        // version bin dirs contain `node` as well).
        if let Some(bin_dir) = server.parent() {
            let inherited = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{inherited}", bin_dir.display()));
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("failed to spawn {}: {e}", server.display()))?;

        let stdin: ChildStdin = child
            .stdin
            .take()
            .ok_or_else(|| "server stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "server stdout unavailable".to_string())?;

        let (diagnostics_tx, diagnostics_rx) = unbounded();
        let (writer_tx, writer_rx) = unbounded::<String>();

        let inner = Arc::new(Inner {
            next_id: AtomicI64::new(1),
            writer_tx,
            pending: Mutex::new(HashMap::new()),
            diagnostics_tx,
            alive: AtomicBool::new(true),
            child: Mutex::new(Some(child)),
        });

        // Writer thread: serialize framed messages into the server's stdin.
        std::thread::spawn(move || {
            let mut stdin = stdin;
            for frame in writer_rx.iter() {
                if stdin.write_all(frame.as_bytes()).is_err() {
                    break;
                }
                if stdin.flush().is_err() {
                    break;
                }
            }
        });

        // Reader thread: parse frames, dispatch responses/notifications.
        let reader_inner = inner.clone();
        let reply_tx = inner.writer_tx.clone();
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            loop {
                // Read headers until the blank line.
                let mut content_length: Option<usize> = None;
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => return mark_dead(&reader_inner),
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                break;
                            }
                            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                                content_length = rest.trim().parse().ok();
                            }
                        }
                    }
                }
                let Some(len) = content_length else {
                    return mark_dead(&reader_inner);
                };
                let mut body = vec![0u8; len];
                if reader.read_exact(&mut body).is_err() {
                    return mark_dead(&reader_inner);
                }
                let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
                    continue;
                };
                dispatch(msg, &reader_inner, &reply_tx);
            }
        });

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

fn mark_dead(inner: &Inner) {
    inner.alive.store(false, Ordering::SeqCst);
}

/// Locate the `typescript-language-server` binary. GUI launches inherit a
/// minimal PATH (`/usr/bin:/bin`…), so besides the current PATH also probe
/// the well-known locations of version managers (fnm, nvm, volta) and
/// global npm prefixes.
fn find_server_binary() -> Result<PathBuf, String> {
    const NAME: &str = "typescript-language-server";
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();

    if let Ok(path_env) = std::env::var("PATH") {
        dirs.extend(std::env::split_paths(&path_env));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));

    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".volta/bin"));
        // fnm: ~/.local/share/fnm/node-versions/<ver>/installation/bin
        let fnm_dir = std::env::var_os("FNM_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(home.join(".local/share/fnm")));
        if let Some(fnm) = fnm_dir {
            push_version_bins(&fnm.join("node-versions"), "installation/bin", &mut dirs);
        }
        // nvm: ~/.nvm/versions/node/<ver>/bin
        push_version_bins(&home.join(".nvm/versions/node"), "bin", &mut dirs);
    }

    dirs.into_iter()
        .map(|dir| dir.join(NAME))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            "typescript-language-server not found (searched PATH, fnm, nvm, volta, homebrew)"
                .to_string()
        })
}

/// Add `<root>/<version>/<suffix>` for every version directory under `root`.
fn push_version_bins(root: &Path, suffix: &str, dirs: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            dirs.push(entry.path().join(suffix));
        }
    }
}

/// Classify an inbound server message: response to our request, a server
/// request (answered with a null result), or a notification.
fn dispatch(msg: Value, inner: &Inner, reply_tx: &Sender<String>) {
    let id = msg.get("id").and_then(Value::as_i64);

    // Response to one of our requests.
    if let Some(id) = id {
        if msg.get("method").is_none() {
            let responder = inner.pending.lock().remove(&id);
            let result = if let Some(err) = msg.get("error") {
                Err(err.to_string())
            } else {
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            };
            if let Some(tx) = responder {
                let _ = tx.send(result);
            }
            return;
        }
    }

    match msg.get("method").and_then(Value::as_str) {
        // Server-initiated request: reply with an empty result so the server
        // is never left waiting (tsserver occasionally asks for
        // configuration / capability registration).
        Some(_) if id.is_some() => {
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": null});
            let _ = reply_tx.send(format!(
                "Content-Length: {}\r\n\r\n{}",
                reply.to_string().len(),
                reply
            ));
        }
        Some("textDocument/publishDiagnostics") => {
            if let Ok(params) =
                serde_json::from_value::<PublishDiagnosticsParams>(msg["params"].clone())
            {
                let _ = inner.diagnostics_tx.send(PublishedDiagnostics {
                    uri: params.uri.to_string(),
                    diagnostics: params.diagnostics,
                });
            }
        }
        _ => {}
    }
}

/// Normalize a textDocument/definition result (`Location`, `Location[]`,
/// `LocationLink[]` or null) into `Vec<LocationLink>`.
fn location_links_from_value(value: Value) -> Vec<LocationLink> {
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
pub fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded: String = rest
        .split(|c| c == '/' || c == '?')
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
    Some(std::path::PathBuf::from(decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "scripter-lsp-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn position(line: u32, character: u32) -> lsp_types::Position {
        lsp_types::Position { line, character }
    }

    #[test]
    fn uri_roundtrip() {
        let dir = std::env::temp_dir().join("some dir with spaces");
        let uri = path_to_uri(&dir);
        assert!(uri.starts_with("file:///"));
        assert!(!uri.contains(' '));
        let back = uri_to_path(&uri).unwrap();
        assert_eq!(back, dir);
    }

    #[test]
    fn initialize_diagnostics_hover_completion() {
        let dir = unique_temp_dir("init");
        let Ok(client) = LspClient::spawn(&dir) else {
            eprintln!("SKIP: typescript-language-server not installed");
            return;
        };
        assert!(client.is_alive());

        // Open a document with an obvious type error.
        let uri = path_to_uri(&dir.join("bad.ts"));
        client.did_open(
            &uri,
            "typescript",
            "const n: number = \"oops\";\nconsole.log(n);\n",
        );

        // Wait for publishDiagnostics (tsserver needs a moment).
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut got = None;
        while Instant::now() < deadline {
            for published in client.take_diagnostics() {
                if published.uri == uri && !published.diagnostics.is_empty() {
                    got = Some(published);
                }
            }
            if got.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let published = got.expect("expected diagnostics for the type error");
        assert!(
            published
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Type") || d.message.contains("string")),
            "unexpected diagnostics: {:?}",
            published.diagnostics
        );

        // Hover over `console` should produce something.
        client.did_change(&uri, 1, "console.log(1);\n");
        std::thread::sleep(Duration::from_millis(500));
        let hover = client
            .hover(&uri, position(0, 3))
            .expect("hover request should not fail");
        assert!(hover.is_some(), "expected hover info for `console`");

        // Completion after `console.` should contain `log`.
        client.did_change(&uri, 2, "console.\n");
        std::thread::sleep(Duration::from_millis(500));
        let completions = client
            .completion(&uri, position(0, 8), None)
            .expect("completion request should not fail");
        let items = match completions {
            lsp_types::CompletionResponse::Array(items) => items,
            lsp_types::CompletionResponse::List(list) => list.items,
        };
        assert!(
            items.iter().any(|i| i.label == "log"),
            "expected `log` in completions, got: {:?}",
            items.iter().map(|i| &i.label).take(20).collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
