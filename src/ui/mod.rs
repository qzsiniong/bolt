//! Cross-module UI pieces: the host-interaction overlay, the window pin
//! control, and the pure text-edit operations the editor panel binds to.
//! Per-feature views (script manager, launcher, JSON editor) live under
//! `src/modules/<feature>/`.

pub(crate) mod editor_ops;
pub(crate) mod interaction_window;
pub(crate) mod window_pin;
