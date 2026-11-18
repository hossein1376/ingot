//! The dependency cache's location (05 §33.2). Only the command layer reads the
//! environment; the loader is given the directory in [`crate::loader::LoadOptions`].

use std::path::PathBuf;

/// `$INGOT_CACHE`, or else `~/.cache/ingot`.
pub fn default_cache() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("INGOT_CACHE") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".cache").join("ingot"))
}
