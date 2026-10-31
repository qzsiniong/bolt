//! 编辑器面板的编辑快捷键：每个 action 计算一个纯文本变换并应用到编辑器。

use gpui_kit::component::input::Rope;
use gpui_kit::{Context, Window};

use super::EditorPanel;
use crate::ui::editor_ops;

// 编辑器的按键上下文是 "Input mode=editor"，所以挂在 "Input" 下的绑定只在
// 编辑器聚焦时触发。注意块注释不能用 `Cmd+Shift+/`：macOS 上 shift 后的
// 标点键上报为 `cmd-?`，不是 `cmd-shift-/`。
gpui_kit::actions!(
    bolt,
    [
        ToggleComment,
        ToggleBlockComment,
        DeleteLine,
        DuplicateLineUp,
        DuplicateLineDown,
        MoveLineUp,
        MoveLineDown,
    ]
);

impl EditorPanel {
    /// 对编辑器应用纯文本变换：基于当前 rope/选区算出一个 edit，替换该
    /// 字节范围并设置 resulting selection。先改选区可以把
    /// [`EditorState::replace`] 当成替换任意范围的 primitive 用。`replace`
    /// 照常发 `InputEvent::Change`，所以 dirty 追踪和 LSP `did_change`
    /// 同步不受影响。
    fn apply_op(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        compute: impl FnOnce(&Rope, std::ops::Range<usize>) -> Option<editor_ops::Edit>,
    ) {
        if self.current_script.is_none() {
            return;
        }
        self.editor.update(cx, |state, ecx| {
            let Some(edit) = compute(state.text(), state.selected_range()) else {
                return;
            };
            // 无操作（no-op）的 edit 直接跳过，免得多记一步撤销。
            if edit.text == state.text().slice(edit.range.clone()) {
                return;
            }
            state.set_selected_range(edit.range.clone(), ecx);
            state.replace(edit.text.clone(), window, ecx);
            state.set_selected_range(edit.selection.clone(), ecx);
        });
    }

    pub(super) fn on_toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, editor_ops::toggle_line_comment);
    }

    pub(super) fn on_toggle_block_comment(
        &mut self,
        _: &ToggleBlockComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, editor_ops::toggle_block_comment);
    }

    pub(super) fn on_delete_line(
        &mut self,
        _: &DeleteLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, editor_ops::delete_lines);
    }

    pub(super) fn on_duplicate_line_up(
        &mut self,
        _: &DuplicateLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::duplicate_lines(rope, selection, true)
        });
    }

    pub(super) fn on_duplicate_line_down(
        &mut self,
        _: &DuplicateLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::duplicate_lines(rope, selection, false)
        });
    }

    pub(super) fn on_move_line_up(
        &mut self,
        _: &MoveLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::move_lines(rope, selection, true)
        });
    }

    pub(super) fn on_move_line_down(
        &mut self,
        _: &MoveLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::move_lines(rope, selection, false)
        });
    }
}
