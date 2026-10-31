//! 内建宿主模块，以及它们背后的原生实现。

pub(crate) mod bolt;
pub(crate) mod qr;
pub(crate) mod typer;

use super::HostRegistry;

/// 注册全部内建宿主模块。
pub fn register_all(registry: &mut HostRegistry) {
    bolt::register(registry);
}
