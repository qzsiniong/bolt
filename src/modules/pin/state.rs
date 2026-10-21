//! Pin 模块的应用级状态：叠加偏移计数器。
//!
//! 不记录活跃便签的窗口句柄：便签的生命周期完全由窗口自己管（双击关闭 =
//! `window.remove_window()`），模块侧跟踪句柄只会引入 `&mut App` 借用冲突与
//! 死代码，收益为零。这里只保留「新便签该往右下叠多少」这一个计数器。

use gpui_kit::{Global, Pixels, px};

/// Pin 模块状态，随 app 生命周期存活的 Global。
pub(crate) struct PinState {
    /// 叠加层偏移计数器：新 pin 出现在 mouse + offset，每开一个 +1，到 6 归零。
    offset_index: u8,
}

impl Global for PinState {}

impl PinState {
    pub(crate) fn new() -> Self {
        Self { offset_index: 0 }
    }

    /// 叠加偏移：每开一个 pin 在鼠标位置基础上再偏移一点，避免叠在一起。
    /// 6 层一个轮回，第 7 层回到原点。
    pub(crate) fn next_offset(&mut self) -> (Pixels, Pixels) {
        const LAYER: f32 = 24.;
        let index = self.offset_index % 6;
        self.offset_index = self.offset_index.saturating_add(1);
        (px(index as f32 * LAYER), px(index as f32 * LAYER))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_progress_and_wrap() {
        let mut state = PinState::new();
        let first = state.next_offset();
        assert_eq!(first.0, px(0.));
        let second = state.next_offset();
        assert_eq!(second.0, px(24.));
        // 走完 6 层回到原点。
        for _ in 0..4 {
            state.next_offset();
        }
        let wrapped = state.next_offset();
        assert_eq!(wrapped.0, px(0.));
    }
}
