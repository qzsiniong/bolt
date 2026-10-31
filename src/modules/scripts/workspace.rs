use std::sync::Arc;

use gpui_kit::component::dock::{DockArea, DockLayout, DockSkin, panel_handle};
use gpui_kit::{
    AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    Render, Styled, Subscription, Window, div, px,
};

use super::panels::editor::{RunRequested, StopRequested};
use super::panels::script_list_panel::ScriptSelected;
use super::panels::{EditorPanel, ScriptListPanel};
use crate::modules::console::panel::ConsolePanel;
use crate::runtime::run_service::{RunService, RunStateChanged};
use crate::ui::window::{WindowPin, WindowTitleBar};
use bolt_console::ConsoleLog;

gpui_kit::actions!(bolt, [SaveScript, RunScript, NewScript]);

/// 脚本管理器窗口：带脚本列表、编辑器与控制台的 dock 布局。脚本执行在
/// app 级的 [`RunService`] 里跑，因此本窗口可以随时关掉，不影响从
/// Launcher 启动的运行。
pub struct Workspace {
    /// 标题栏右上角的置顶图钉（公共控件，读窗口真实层级）。
    pin: Entity<WindowPin>,
    /// dock 区（左：脚本列表；右上：编辑器；右下：控制台）。
    dock_area: Entity<DockArea>,
    script_list: Entity<ScriptListPanel>,
    editor: Entity<EditorPanel>,
    run_service: Entity<RunService>,
    _sub_select: Subscription,
    _sub_run: Subscription,
    _sub_stop: Subscription,
    _sub_run_state: Subscription,
}

impl Workspace {
    /// 组装窗口：创建列表 / 编辑器 / 控制台三个面板并按 dock 布局拼装，
    /// 订阅选中、运行、停止事件。
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        lsp: Result<Arc<bolt_lsp::LspClient>, String>,
        console_log: ConsoleLog,
        run_service: Entity<RunService>,
    ) -> Self {
        // 窗口级快捷键：Cmd+S 保存、Cmd+Enter 运行、Cmd+N 新建。
        cx.bind_keys([
            KeyBinding::new("cmd-s", SaveScript, None),
            KeyBinding::new("cmd-enter", RunScript, None),
            KeyBinding::new("cmd-n", NewScript, None),
        ]);

        let dock_area = cx
            .new(|cx| DockArea::new("main", Some(1), window, cx).with_renderer(DockSkin::new(cx)));

        let script_list = cx.new(|cx| ScriptListPanel::new(window, cx));
        let console = cx.new(|cx| ConsolePanel::new(window, console_log, cx));
        let editor = cx.new(|cx| EditorPanel::new(window, cx, lsp.ok()));
        // 让列表面板在替换编辑器里当前打开的脚本之前，先对未保存的编辑给出警告。
        script_list.update(cx, |list, _| list.set_editor(editor.downgrade()));

        // 选中脚本 → 载入编辑器（推迟到 render：EditorState::set_value 需要
        // window，事件回调里没有）。
        let sub_select = cx.subscribe(
            &script_list,
            move |this, _list, event: &ScriptSelected, cx| {
                this.editor
                    .update(cx, |ed, cx| ed.queue_load(event.0.clone(), cx));
            },
        );

        // 运行请求 → 把脚本交给 app 级 run service。
        let sub_run = cx.subscribe(&editor, move |this, _ed, event: &RunRequested, cx| {
            this.run_service.update(cx, |svc, cx| {
                svc.start_script(
                    event.content.clone(),
                    event.language,
                    event.file_name.clone(),
                    cx,
                );
            });
        });

        // 停止请求 → 翻转服务的取消 token。
        let sub_stop = cx.subscribe(&editor, move |this, _ed, _: &StopRequested, cx| {
            this.run_service.update(cx, |svc, _| svc.stop());
        });

        // 可能已有运行在进行（从 Launcher 启动的）：工具栏要立即反映出来，
        // 而不是等下一个事件。
        editor.update(cx, |ed, cx| {
            ed.set_running(run_service.read(cx).is_running(), cx)
        });

        // 跟随服务的运行状态：任何来源启动的运行（编辑器按钮、Cmd+Enter 或
        // Launcher）都能反映到工具栏。
        let sub_run_state = cx.subscribe(
            &run_service,
            move |this, _svc, event: &RunStateChanged, cx| {
                this.editor.update(cx, |ed, cx| ed.set_running(event.0, cx));
            },
        );

        dock_area.update(cx, |area, dock_cx| {
            let layout = DockLayout::h_split()
                .child(
                    DockLayout::tabs().panel_view(panel_handle(script_list.clone()), dock_cx),
                    Some(px(240.)),
                )
                .child(
                    DockLayout::v_split()
                        .child(
                            DockLayout::tabs().panel_view(panel_handle(editor.clone()), dock_cx),
                            None,
                        )
                        .child(
                            DockLayout::tabs().panel_view(panel_handle(console.clone()), dock_cx),
                            Some(px(200.)),
                        ),
                    None,
                );

            area.set_center(layout, window, dock_cx);
        });

        Self {
            pin: cx.new(|_| WindowPin::new()),
            dock_area,
            script_list,
            editor,
            run_service,
            _sub_select: sub_select,
            _sub_run: sub_run,
            _sub_stop: sub_stop,
            _sub_run_state: sub_run_state,
        }
    }

    /// 走一遍新建脚本流程（供 Launcher 的 NewScript 命令调用）。
    pub fn trigger_new_script(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.script_list
            .update(cx, |list, cx| list.create_new(window, cx));
    }

    fn on_save(&mut self, _: &SaveScript, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| ed.save(window, cx));
    }

    fn on_run(&mut self, _: &RunScript, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| ed.run_current(window, cx));
    }

    fn on_new(&mut self, _: &NewScript, window: &mut Window, cx: &mut Context<Self>) {
        self.script_list
            .update(cx, |list, cx| list.create_new(window, cx));
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 让系统级窗口标题跟随当前打开的脚本（Mission Control / Cmd+Tab 里
        // 看得到）。屏幕上显示的标题在下面的自绘标题栏里。
        let title = match self.editor.read(cx).current_script() {
            Some(script) => format!("Bolt — {}", script.file_name),
            None => "Bolt".to_string(),
        };
        window.set_window_title(&title);

        // 展示层副标题：当前打开的脚本名（与系统级标题同一个信息源）。
        let subtitle = self
            .editor
            .read(cx)
            .current_script()
            .map(|script| script.file_name.clone())
            .unwrap_or_else(|| "未打开脚本".to_string());

        div()
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_run))
            .on_action(cx.listener(Self::on_new))
            .child(
                WindowTitleBar::new("Bolt")
                    .subtitle(subtitle)
                    .pin(self.pin.clone()),
            )
            .child(div().flex_1().min_h_0().child(self.dock_area.clone()))
    }
}
