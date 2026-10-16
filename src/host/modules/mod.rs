//! Built-in host modules and the native implementations behind them.

pub(crate) mod qr;
pub(crate) mod scripter;
pub(crate) mod typer;

use super::HostRegistry;

/// Register all built-in host modules.
pub fn register_all(registry: &mut HostRegistry) {
    scripter::register(registry);
}
