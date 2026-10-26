//! 屏幕取色模块：全屏放大镜拾色，复制 HEX 色值。
//!
//! 接入 Launcher 提供一条「屏幕取色」命令。confirm 后（launcher 窗口已关）
//! 打开覆盖鼠标所在显示器的透明浮窗，详见 [`window`]。

pub(crate) mod capture;
pub(crate) mod window;

use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::assets::IconName;

use crate::app::launcher::{LauncherItem, Module};

/// 屏幕取色：只贡献一条「屏幕取色」内置命令。
pub(crate) struct ColorPickerModule;

impl Module for ColorPickerModule {
    fn name(&self) -> &'static str {
        "color_picker"
    }

    fn collect_items(&self, _cx: &App) -> Vec<LauncherItem> {
        vec![LauncherItem {
            id: "color_picker.pick".to_string(),
            title: "屏幕取色".to_string(),
            subtitle: Some("用放大镜拾取屏幕颜色，复制 HEX 色值（Esc 取消）".to_string()),
            icon: IconName::Pipette.into(),
            group: "内置".to_string(),
            keywords: vec![
                "取色器".to_string(),
                "拾色".to_string(),
                "颜色".to_string(),
                "color".to_string(),
                "picker".to_string(),
                "hex".to_string(),
            ],
            on_confirm: Arc::new(window::open),
            actions: Vec::new(),
        }]
    }
}
