//! Web assets compiled into the binary.

/// The page served at `/`.
pub(crate) const INDEX_HTML: &str = include_str!("../web/index.html");
/// Its behaviour.
pub(crate) const APP_JS: &str = include_str!("../web/app.js");
