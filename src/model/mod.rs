//! Domain data and on-disk layout: scripts and the generated script-project
//! files. Extensions live in `modules::extensions::manifest`; the launcher's
//! item contract lives in `app::launcher`.
pub(crate) mod project_gen;
pub(crate) mod script;
pub(crate) mod script_store;
