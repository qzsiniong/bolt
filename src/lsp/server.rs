//! Language-server process management: binary discovery, the stdin writer and
//! stdout reader threads, and inbound message dispatch.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};
use lsp_types::PublishDiagnosticsParams;
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::client::Inner;
use super::protocol::PublishedDiagnostics;

/// Spawn `typescript-language-server --stdio` rooted at `scripts_dir` and start
/// the reader/writer threads around it.
///
/// Returns the shared state, the diagnostics channel, and the resolved server
/// binary path (the caller uses it to locate the matching TypeScript package).
/// `Err` carries the reason when the binary is missing or the process cannot be
/// started — callers degrade gracefully to a syntax-highlighting-only editor.
pub(super) fn spawn_process(
    scripts_dir: &Path,
) -> Result<(Arc<Inner>, Receiver<PublishedDiagnostics>, PathBuf), String> {
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

    Ok((inner, diagnostics_rx, server))
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
    let mut dirs: Vec<PathBuf> = Vec::new();

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
    if let Some(id) = id
        && msg.get("method").is_none()
    {
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
