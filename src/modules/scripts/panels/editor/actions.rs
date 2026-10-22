//! Editing shortcuts for the editor panel: each action computes a pure text
//! transform and applies it to the editor.

use gpui_kit::component::input::Rope;
use gpui_kit::*;

use super::EditorPanel;
use crate::ui::editor_ops;

// The editor's key context is "Input mode=editor", so a binding under "Input"
// only fires while the editor has focus. Note that `Cmd+Shift+/` cannot be
// used for the block comment: on macOS the shifted punctuation key is reported
// as `cmd-?`, not `cmd-shift-/`.
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
    /// Apply a pure text transform to the editor: compute an edit from the
    /// current rope/selection, then replace that byte range and set the
    /// resulting selection. Changing the selection first turns
    /// [`EditorState::replace`] into a replace-arbitrary-range primitive.
    /// `replace` still emits `InputEvent::Change`, so dirty tracking and the
    /// LSP `did_change` sync keep working as usual.
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
            // Guard against no-op edits so they don't add a stray undo step.
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
