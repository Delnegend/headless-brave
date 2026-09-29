//! Web assets compiled into the binary.

/// The page served at `/`.
pub const INDEX_HTML: &str = include_str!("../web/index.html");
/// Its behaviour.
pub const APP_JS: &str = include_str!("../web/app.js");
