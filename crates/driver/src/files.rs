//! Filesystem access for the loader, with `serve` overlays (tools.md §6).
//!
//! A server keeps text for files the editor has not saved. Every read the loader does
//! goes through [`Files`], which consults the overlays first: module files,
//! `ingot.toml`, single files and the standard library. An overlay for a file that is
//! not on disk makes the file exist and appear in its directory's listing; it does not
//! create a directory.
//!
//! Overlay keys are normalised here, never by callers: a key is an absolute path whose
//! directory part is canonical, so `/tmp/x` and `/private/tmp/x` name the same file on
//! macOS and a file that does not exist yet still has a key.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Overlays {
    files: BTreeMap<PathBuf, String>,
}

impl Overlays {
    pub fn new() -> Self {
        Overlays::default()
    }

    /// Use `text` in place of the contents of `path` until it is cleared.
    pub fn insert(&mut self, path: &Path, text: String) {
        self.files.insert(normalize(path), text);
    }

    /// Clear the overlay of `path`, if there is one.
    pub fn remove(&mut self, path: &Path) {
        self.files.remove(&normalize(path));
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn get(&self, path: &Path) -> Option<&str> {
        if self.files.is_empty() {
            return None;
        }
        self.files.get(&normalize(path)).map(String::as_str)
    }

    fn keys(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.keys()
    }
}

/// The normalised key of `path`: an absolute path whose directory is canonical. A
/// directory that does not exist cannot be canonicalised, so the absolute path is used
/// unchanged.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        return absolute;
    };
    match parent.canonicalize() {
        Ok(canonical) => canonical.join(name),
        Err(_) => absolute,
    }
}

/// The files the loader sees: the disk, with overlays in front.
#[derive(Clone, Debug, Default)]
pub struct Files {
    overlays: Overlays,
}

impl Files {
    pub fn new(overlays: Overlays) -> Self {
        Files { overlays }
    }

    pub fn overlays(&self) -> &Overlays {
        &self.overlays
    }

    pub fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if let Some(text) = self.overlays.get(path) {
            return Ok(text.as_bytes().to_vec());
        }
        std::fs::read(path)
    }

    pub fn read_to_string(&self, path: &Path) -> io::Result<String> {
        if let Some(text) = self.overlays.get(path) {
            return Ok(text.to_string());
        }
        std::fs::read_to_string(path)
    }

    /// True for a file on disk or a file that an overlay makes exist.
    pub fn exists(&self, path: &Path) -> bool {
        self.overlays.get(path).is_some() || path.exists()
    }

    /// True for a file on disk or a file that an overlay makes exist.
    pub fn is_file(&self, path: &Path) -> bool {
        self.overlays.get(path).is_some() || path.is_file()
    }

    /// An overlay never creates a directory: only the disk decides.
    pub fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    /// The entries of `dir`: the disk's, with overlaid files merged in, sorted by path.
    /// A directory that is not on disk cannot be listed.
    pub fn read_dir(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            paths.push(entry?.path());
        }
        if !self.overlays.is_empty() {
            let dir_key = dir.canonicalize().ok();
            if let Some(dir_key) = dir_key {
                for key in self.overlays.keys() {
                    if let (Some(parent), Some(name)) = (key.parent(), key.file_name())
                        && parent == dir_key
                    {
                        paths.push(dir.join(name));
                    }
                }
            }
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ingot-files-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_overlay_is_read_instead_of_the_disk_and_can_be_cleared() {
        let dir = scratch("read");
        let path = dir.join("a.ingot");
        std::fs::write(&path, "on disk").unwrap();
        let mut overlays = Overlays::new();
        overlays.insert(&path, "in memory".to_string());
        let files = Files::new(overlays.clone());
        assert_eq!(files.read_to_string(&path).unwrap(), "in memory");
        overlays.remove(&path);
        let files = Files::new(overlays);
        assert_eq!(files.read_to_string(&path).unwrap(), "on disk");
    }

    #[test]
    fn a_file_that_is_not_on_disk_exists_and_is_listed() {
        let dir = scratch("listing");
        std::fs::write(dir.join("a.ingot"), "").unwrap();
        let mut overlays = Overlays::new();
        overlays.insert(&dir.join("b.ingot"), "fn b() {}".to_string());
        let files = Files::new(overlays);
        let missing = dir.join("b.ingot");
        assert!(files.exists(&missing));
        assert!(files.is_file(&missing));
        assert!(!files.is_dir(&missing));
        assert_eq!(files.read_to_string(&missing).unwrap(), "fn b() {}");
        let names: Vec<String> = files
            .read_dir(&dir)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.ingot", "b.ingot"]);
    }

    #[test]
    fn an_overlay_does_not_create_a_directory() {
        let dir = scratch("dir");
        let mut overlays = Overlays::new();
        overlays.insert(&dir.join("missing/b.ingot"), String::new());
        let files = Files::new(overlays);
        assert!(!files.is_dir(&dir.join("missing")));
        assert!(files.read_dir(&dir.join("missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_has_one_key() {
        let dir = scratch("symlink");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut overlays = Overlays::new();
        overlays.insert(&link.join("a.ingot"), "through the link".to_string());
        let files = Files::new(overlays);
        assert_eq!(
            files.read_to_string(&real.join("a.ingot")).unwrap(),
            "through the link"
        );
        let names: Vec<PathBuf> = files.read_dir(&real).unwrap();
        assert_eq!(names, [real.join("a.ingot")]);
    }
}
