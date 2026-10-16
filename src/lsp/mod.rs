//! Minimal LSP client speaking JSON-RPC over stdio with
//! `typescript-language-server`. Runs the server as a child process and
//! bridges requests/notifications through blocking threads, so callers
//! (GPUI background tasks) use plain channel waits.
//!
//! `client` holds the handle and request plumbing, `server` the child process
//! and its reader/writer threads, `protocol` the wire vocabulary, and
//! `providers` the bridge into gpui-kit's editor provider traits.

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

        // Open a document with an obvious type error.
        let uri = path_to_uri(&dir.0.join("bad.ts"));
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
    }

    /// Regression test: when the user types after a member-access trigger, the
    /// tsserver must still return useful completions even if the LSP client
    /// sends a multi-character trigger_character (gpui editor sends the whole
    /// query as trigger_character). The provider's own filtering should then
    /// narrow the list to matching members.
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

        // 1) After `.` — tsserver should return member completions with `log`.
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

        // 2) After `console.l` — tsserver may receive trigger_character `.l`.
        //    It should still return members (or a subset); client-side filtering
        //    must then keep `log`.
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

        // 3) After `console` — plain-word trigger. tsserver returns scope-wide
        //    identifiers. Client-side filtering should keep only `console`.
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
