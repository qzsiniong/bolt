//! 编辑器面板的文档生命周期：从磁盘加载脚本、保存回磁盘、运行，以及把
//! 语言服务推送的诊断镜像进编辑器。

use std::time::Duration;

use gpui_kit::component::WindowExt;
use gpui_kit::{AsyncApp, Context, WeakEntity, Window};

use super::{EditorPanel, RunRequested};
use crate::app::shell::AppShell;
use crate::lsp::path_to_uri;
use crate::lsp::providers::DocHandle;
use crate::model::script::Script;
use crate::model::script_store;

impl EditorPanel {
    /// 把当前编辑器内容整段推给语言服务（全量同步）。
    pub(super) fn sync_did_change(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.as_ref() else {
            return;
        };
        let text = self.editor.read(cx).value().to_string();
        let mut doc = self.doc.lock();
        let Some(handle) = doc.as_mut() else {
            return;
        };
        handle.version += 1;
        client.did_change(&handle.uri, handle.version, &text);
    }

    /// 轮询语言服务推送的诊断，镜像进编辑器当前打开文档的诊断集
    /// （波浪线标记）。
    pub(super) async fn diagnostics_loop(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let alive = this
                .update(cx, |panel, cx| {
                    let Some(client) = panel.lsp.clone() else {
                        return;
                    };
                    let current_uri = panel.doc.lock().as_ref().map(|d| d.uri.clone());
                    let mut changed = false;
                    for published in client.take_diagnostics() {
                        if current_uri.as_deref() != Some(published.uri.as_str()) {
                            continue;
                        }
                        panel.editor.update(cx, |state, ecx| {
                            let rope = state.text().clone();
                            if let Some(set) = state.diagnostics_mut() {
                                // `reset` 会同步内部 rope，并在推入新诊断前先清掉过期诊断。
                                set.reset(&rope);
                                for diagnostic in published.diagnostics {
                                    set.push(diagnostic);
                                }
                                state.refresh(ecx);
                            }
                        });
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        }
    }

    pub fn current_script(&self) -> Option<&Script> {
        self.current_script.as_ref()
    }

    /// 记录从这里启动的脚本是否正在执行。由宿主（Workspace）在运行开始与
    /// 结束时驱动，工具栏据此在「运行 / 停止」之间切换。
    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        if self.running != running {
            self.running = running;
            cx.notify();
        }
    }

    /// 编辑器内容是否与磁盘上已保存的脚本不同。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 把脚本排进队列，等下一次渲染时加载（渲染才有 window 可用，
    /// `set_value` 需要它——subscribe 回调拿不到）。
    pub fn queue_load(&mut self, script: Script, cx: &mut Context<Self>) {
        self.pending_load = Some(script);
        cx.notify();
    }

    /// 把脚本内容载入编辑器并切换语法高亮。
    ///
    /// 未保存的编辑**不会**在这里自动保存：切走即丢弃——「未保存的修改
    /// 将会丢失」确认框警告的就是这件事。
    pub(super) fn load_script(
        &mut self,
        script: &Script,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 列表面板持有的是过期快照，所以总是从磁盘重读，拿列表构建之后
        // 保存过的最新内容。
        let script = match script_store::read_script(&self.scripts_dir, &script.file_name) {
            Ok(fresh) => fresh,
            Err(_) => script.clone(),
        };
        self.current_script = Some(script.clone());
        self.dirty = false;

        // 同步 LSP 文档：关旧的，开新的。
        let language_id = script.language.as_str();
        let uri = path_to_uri(&script.path);
        if let Some(client) = self.lsp.as_ref() {
            let old_uri = self.doc.lock().as_ref().map(|d| d.uri.clone());
            if let Some(old) = old_uri.filter(|old| *old != uri) {
                client.did_close(&old);
            }
            client.did_open(&uri, language_id, &script.content);
            *self.doc.lock() = Some(DocHandle { uri, version: 0 });
        }

        let lang = script.language;
        let content = script.content.clone();
        self.editor.update(cx, |state, cx| {
            state.set_value(content.as_str(), window, cx);
            state.set_highlighter(lang.as_str(), cx);
            // 诊断集的内部 rope 必须与新文本一致，后续诊断做
            // position -> offset 映射才正确。
            let rope = state.text().clone();
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&rope);
            }
        });
        cx.notify();
    }

    /// 运行当前加载的脚本（工具栏按钮 / Cmd+Enter）。
    pub fn run_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.current_script.is_none() {
            return;
        }
        if self.dirty {
            // 有未保存的编辑：先问要不要保存。「否」直接运行磁盘上
            // 最后保存的版本，而不是编辑器里的当前文本。
            let editor = cx.entity();
            window.open_alert_dialog(cx, move |alert, _, _| {
                alert
                    .title("未保存的修改")
                    .description(
                        "脚本有未保存的修改。先保存再运行吗？（否 = 直接运行已保存的版本）",
                    )
                    .show_cancel(true)
                    .ok_text("保存并运行")
                    .cancel_text("否，运行已保存版本")
                    .on_ok({
                        let editor = editor.clone();
                        move |_, window, cx| {
                            editor.update(cx, |ed, ecx| {
                                ed.save(window, ecx);
                                ed.emit_run_edited(ecx);
                            });
                            true
                        }
                    })
                    .on_cancel({
                        let editor = editor.clone();
                        move |_, _, cx| {
                            editor.update(cx, |ed, ecx| ed.emit_run_saved(ecx));
                            true
                        }
                    })
            });
            return;
        }
        self.emit_run_edited(cx);
    }

    /// 请宿主运行编辑器当前文本。
    fn emit_run_edited(&mut self, cx: &mut Context<Self>) {
        let Some(script) = self.current_script.clone() else {
            return;
        };
        let content = self.editor.read(cx).value().to_string();
        cx.emit(RunRequested {
            content,
            language: script.language,
            file_name: script.file_name,
        });
    }

    /// 请宿主运行磁盘上最后保存的版本，忽略未保存的编辑。
    fn emit_run_saved(&mut self, cx: &mut Context<Self>) {
        let Some(script) = self.current_script.clone() else {
            return;
        };
        cx.emit(RunRequested {
            content: script.content,
            language: script.language,
            file_name: script.file_name,
        });
    }

    /// 保存当前脚本（工具栏按钮 / Cmd+S）。
    pub fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty || self.current_script.is_none() {
            return;
        }
        self.persist_current(cx);
        cx.notify();
    }

    /// 把编辑器内容写回脚本的磁盘文件。
    fn persist_current(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }
        let Some(script) = self.current_script.clone() else {
            return;
        };
        let content = self.editor.read(cx).value().to_string();
        if let Err(e) = script_store::save_script(&self.scripts_dir, &script.file_name, &content) {
            let console = cx.global::<AppShell>().console_channel.clone();
            console.error(format!("保存脚本失败：{e}"));
            return;
        }
        // 把已保存的内容同步回缓存的 Script。
        if let Some(s) = self.current_script.as_mut() {
            s.content = content;
        }
        self.dirty = false;
    }

    pub(super) fn has_runnable(&self) -> bool {
        self.current_script.is_some()
    }
}
