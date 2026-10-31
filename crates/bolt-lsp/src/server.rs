//! 语言服务器进程管理：可执行文件查找、stdin 写线程与 stdout 读线程，
//! 以及入站消息分发。

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

/// 以 `scripts_dir` 为根启动 `typescript-language-server --stdio`，并在它周围
/// 起读/写线程。
///
/// 返回共享状态、诊断通道，以及解析出的服务器可执行文件路径（调用方用它找到
/// 配套的 TypeScript 包）。可执行文件缺失或进程起不来时 `Err` 带原因——调用方
/// 会优雅降级成只有语法高亮的编辑器。
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
    // 服务器脚本的 shebang 是 `#!/usr/bin/env node`，需要 PATH 里有 `node`。
    // 从 Finder 启动时继承来的 PATH 很短，所以把找到服务器的那个目录前置进去
    // （fnm/nvm 的版本 bin 目录里同样有 `node`）。
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

    // 写线程：把已分帧的消息按序写进服务器的 stdin。
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

    // 读线程：拆帧，分发响应与通知。
    let reader_inner = inner.clone();
    let reply_tx = inner.writer_tx.clone();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        loop {
            // 一直读头部，直到空行为止。
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

/// 找出 `typescript-language-server` 可执行文件。GUI 启动的进程继承的 PATH 很
/// 短（`/usr/bin:/bin`…），所以除当前 PATH 外还要探版本管理器（fnm、nvm、
/// volta）与 npm 全局前缀的常见位置。
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
        // fnm 的版本目录：~/.local/share/fnm/node-versions/<ver>/installation/bin
        let fnm_dir = std::env::var_os("FNM_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(home.join(".local/share/fnm")));
        if let Some(fnm) = fnm_dir {
            push_version_bins(&fnm.join("node-versions"), "installation/bin", &mut dirs);
        }
        // nvm 的版本目录：~/.nvm/versions/node/<ver>/bin
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

/// 为 `root` 下每个版本目录补一个 `<root>/<version>/<suffix>`。
fn push_version_bins(root: &Path, suffix: &str, dirs: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            dirs.push(entry.path().join(suffix));
        }
    }
}

/// 给入站消息分类：对我们请求的响应、服务器发起的请求（回一个 null 结果），
/// 或者是通知。
fn dispatch(msg: Value, inner: &Inner, reply_tx: &Sender<String>) {
    let id = msg.get("id").and_then(Value::as_i64);

    // 我们某条请求的响应。
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
        // 服务器发起的请求：回一个空结果，别让它一直等着（tsserver 偶尔会来
        // 问配置或能力注册）。
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
