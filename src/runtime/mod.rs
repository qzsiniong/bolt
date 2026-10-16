//! The script execution pipeline: the QuickJS runner, the host-interaction
//! bridge, the app-level run service, and the TypeScript type-stripping step.

pub(crate) mod interaction;
pub(crate) mod run_service;
pub(crate) mod runner;
pub(crate) mod ts_transform;
