//! 一个最小 LSP 客户端：以 stdio 上的 JSON-RPC 与
//! `typescript-language-server` 通信。它把语言服务作为子进程跑，请求与通知都
//! 经阻塞线程中转，调用方（GPUI 后台任务）只需普通的通道等待。
//!
//! `client` 是句柄与请求管线，`server` 是子进程及其读/写线程，`protocol` 是
//! 协议词汇，`providers` 是接进 gpui-kit 编辑器 provider trait 的桥。

pub(crate) mod client;
mod protocol;
pub(crate) mod providers;
mod server;

pub use client::LspClient;
pub use protocol::path_to_uri;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDir, position};
    use std::time::{Duration, Instant};

    #[test]
    fn uri_roundtrip() {
        let dir = std::env::temp_dir().join("some dir with spaces");
        let uri = path_to_uri(&dir);
        assert!(uri.starts_with("file:///"));
        assert!(!uri.contains(' '));
        let back = protocol::uri_to_path(&uri).unwrap();
        assert_eq!(back, dir);
    }

    #[test]
    fn initialize_diagnostics_hover_completion() {
        let dir = TempDir::new("init");
        let Ok(client) = LspClient::spawn(&dir.0) else {
            eprintln!("SKIP: typescript-language-server not installed");
            return;
        };
        assert!(client.is_alive());

        // 打开一个明显有类型错误的文档。
        let uri = path_to_uri(&dir.0.join("bad.ts"));
        client.did_open(
            &uri,
            "typescript",
            "const n: number = \"oops\";\nconsole.log(n);\n",
        );

        // 等 publishDiagnostics 到来（tsserver 需要一点时间）。
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

        // 在 `console` 上悬停应当有内容。
        client.did_change(&uri, 1, "console.log(1);\n");
        std::thread::sleep(Duration::from_millis(500));
        let hover = client
            .hover(&uri, position(0, 3))
            .expect("hover request should not fail");
        assert!(hover.is_some(), "expected hover info for `console`");

        // 在 `console.` 之后补全，结果里应当有 `log`。
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
    }

    /// 回归测试：用户在成员访问触发符之后继续打字时，即使 LSP 客户端传来的
    /// trigger_character 有多个字符（gpui 编辑器会把整段查询当成
    /// trigger_character 发过来），tsserver 也必须仍返回可用的补全。随后的
    /// provider 侧过滤负责把列表收窄到匹配的成员。
    #[test]
    fn completion_after_continued_typing() {
        let dir = TempDir::new("completion");
        let Ok(client) = LspClient::spawn(&dir.0) else {
            eprintln!("SKIP: typescript-language-server not installed");
            return;
        };

        let uri = path_to_uri(&dir.0.join("test.ts"));
        client.did_open(&uri, "typescript", "console.\n");
        std::thread::sleep(Duration::from_secs(1));

        // 1) 在 `.` 之后——tsserver 应返回带 `log` 的成员补全。
        let resp = client
            .completion(
                &uri,
                position(0, 8),
                Some(lsp_types::CompletionContext {
                    trigger_kind: lsp_types::CompletionTriggerKind::TRIGGER_CHARACTER,
                    trigger_character: Some(".".to_string()),
                }),
            )
            .expect("completion failed");
        let items = match resp {
            lsp_types::CompletionResponse::Array(items) => items,
            lsp_types::CompletionResponse::List(list) => list.items,
        };
        assert!(
            items.iter().any(|i| i.label == "log"),
            "expected `log` after `console.` trigger={:?}",
            items.iter().map(|i| &i.label).take(20).collect::<Vec<_>>()
        );

        // 2) 在 `console.l` 之后——tsserver 可能收到 trigger_character `.l`。
        //    它仍应返回成员（或其中一部分），再由客户端过滤保住 `log`。
        client.did_change(&uri, 2, "console.l\n");
        std::thread::sleep(Duration::from_millis(500));
        let resp = client
            .completion(
                &uri,
                position(0, 9),
                Some(lsp_types::CompletionContext {
                    trigger_kind: lsp_types::CompletionTriggerKind::TRIGGER_CHARACTER,
                    trigger_character: Some(".l".to_string()),
                }),
            )
            .expect("completion failed");
        let items = match resp {
            lsp_types::CompletionResponse::Array(items) => items,
            lsp_types::CompletionResponse::List(list) => list.items,
        };
        assert!(
            items.iter().any(|i| i.label == "log"),
            "expected `log` after `console.l` trigger={:?}",
            items.iter().map(|i| &i.label).take(20).collect::<Vec<_>>()
        );

        // 3) 在 `console` 之后——纯单词触发。tsserver 会返回整个作用域的标识符，
        //    客户端过滤应当只留下 `console`。
        client.did_change(&uri, 3, "console\n");
        std::thread::sleep(Duration::from_millis(500));
        let resp = client
            .completion(
                &uri,
                position(0, 7),
                Some(lsp_types::CompletionContext {
                    trigger_kind: lsp_types::CompletionTriggerKind::TRIGGER_CHARACTER,
                    trigger_character: Some("console".to_string()),
                }),
            )
            .expect("completion failed");
        let items = match resp {
            lsp_types::CompletionResponse::Array(items) => items,
            lsp_types::CompletionResponse::List(list) => list.items,
        };
        assert!(
            items.iter().any(|i| i.label == "console"),
            "expected `console` after plain-word trigger={:?}",
            items.iter().map(|i| &i.label).take(20).collect::<Vec<_>>()
        );
    }
}
