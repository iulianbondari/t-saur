//! Archive paths are labels, never destinations. They are normalised on the way in and
//! validated again on the way out, so an archive can never write outside the target directory.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// Components a path may have (`docs/V1-CONTRACT.md` §4).
pub const MAX_DEPTH: usize = 256;

const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];

/// Normalise a path for storage: NFC, forward slashes, relative, no `.`/`..`, no drive letters,
/// no NUL/control characters, no `:` (NTFS alternate data streams), no Windows reserved names,
/// no trailing spaces or dots in components, at most 4096 bytes.
pub fn normalize(raw: &str) -> Result<String> {
    let s: String = raw.replace('\\', "/").nfc().collect();
    let s = s.trim_start_matches("./");
    if s.is_empty() {
        return Err(Error::Policy("empty path".into()));
    }
    if s.starts_with('/') {
        return Err(Error::Policy(format!("absolute path rejected: {raw}")));
    }
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return Err(Error::Policy(format!("drive letter rejected: {raw}")));
    }
    if s.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(Error::Policy(format!("control character in path: {raw:?}")));
    }
    if s.contains(':') {
        return Err(Error::Policy(format!("colon (alternate data stream) rejected: {raw}")));
    }
    let mut parts = Vec::new();
    for comp in s.split('/') {
        if comp.is_empty() {
            return Err(Error::Policy(format!("empty path component: {raw}")));
        }
        if comp == "." || comp == ".." {
            return Err(Error::Policy(format!("dot component rejected: {raw}")));
        }
        if comp.ends_with(' ') || comp.ends_with('.') {
            return Err(Error::Policy(format!("trailing space/dot rejected: {raw}")));
        }
        let stem = comp.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            return Err(Error::Policy(format!("reserved device name rejected: {raw}")));
        }
        parts.push(comp);
    }
    if parts.len() > MAX_DEPTH {
        return Err(Error::Policy(format!("path deeper than {MAX_DEPTH} components")));
    }
    let joined = parts.join("/");
    if joined.len() > 4096 {
        return Err(Error::Policy("path longer than 4096 bytes".into()));
    }
    Ok(joined)
}

/// Join a validated archive path under `root`, component by component.
pub fn safe_join(root: &Path, archive_path: &str) -> Result<PathBuf> {
    let norm = normalize(archive_path)?;
    let mut p = root.to_path_buf();
    for comp in norm.split('/') {
        p.push(comp);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_paths() {
        for bad in ["../x", "a/../b", "/etc/passwd", "C:\\x", "a:b", "CON", "nul.txt", "a/", "x\0y", "dir/..", "name. "] {
            assert!(normalize(bad).is_err(), "should reject {bad:?}");
        }
        let deep = vec!["d"; MAX_DEPTH + 1].join("/");
        assert!(normalize(&deep).is_err(), "deeper than {MAX_DEPTH} components");
        assert!(normalize(&vec!["d"; MAX_DEPTH].join("/")).is_ok());
    }

    #[test]
    fn accepts_and_normalises() {
        assert_eq!(normalize("./docs\\a.md").unwrap(), "docs/a.md");
        assert_eq!(normalize("ok/file.txt").unwrap(), "ok/file.txt");
    }
}
