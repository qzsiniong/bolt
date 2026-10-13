//! Built-in host modules.

pub mod scripter;

use crate::host_module::HostRegistry;

/// Register all built-in host modules.
pub fn register_all(registry: &mut HostRegistry) {
    scripter::register(registry);
}
