//! Paths as the platform, TeX and kpathsea want them.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Separator of search-path variables (`PATH`, `TEXINPUTS`, `BIBINPUTS`, ...).
pub const SEARCH_SEP: &str = if cfg!(windows) { ";" } else { ":" };

/// `canonicalize` without Windows' verbatim prefix: `\\?\C:\x` is `C:\x` and `\\?\UNC\h\s` is
/// `\\h\s`. TeX, kpathsea and most tools cannot open verbatim paths, and they would not compare
/// equal to the same path from `read_dir`. (Canonical paths also expand 8.3 short names such as
/// `RUNNER~1`, whose `~` TeX reads as an active character.)
pub fn canonical(p: &Path) -> std::io::Result<PathBuf> {
    Ok(strip_verbatim(p.canonicalize()?))
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return p;
    }
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// A path written into TeX source or handed to kpathsea: forward slashes (a backslash in TeX
/// source starts a control sequence).
pub fn tex(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// A project-relative file key as the session stores it and TeX reports it: forward slashes
/// (Windows hosts may send `chapters\\a.tex`).
pub fn key(rel: &str) -> String {
    if cfg!(windows) {
        rel.replace('\\', "/")
    } else {
        rel.to_string()
    }
}

/// `first` prepended to the search-path variable `var` of this process. A trailing separator
/// (empty variable) keeps kpathsea's default path.
pub fn prepend_search(first: &str, var: &str) -> String {
    format!(
        "{first}{SEARCH_SEP}{}",
        std::env::var(var).unwrap_or_default()
    )
}

/// Put `dir` (TeX Live's binaries) first on the command's `PATH`.
pub fn prepend_bin_dir(cmd: &mut Command, dir: &Path) {
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        cmd.env("PATH", joined);
    }
}

/// The file name of an executable on this platform (`lualatex.exe` on Windows).
pub fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_stripped_on_windows() {
        let p = strip_verbatim(PathBuf::from(r"\\?\C:\a\b"));
        let u = strip_verbatim(PathBuf::from(r"\\?\UNC\host\share\x"));
        if cfg!(windows) {
            assert_eq!(p, PathBuf::from(r"C:\a\b"));
            assert_eq!(u, PathBuf::from(r"\\host\share\x"));
        } else {
            assert_eq!(p, PathBuf::from(r"\\?\C:\a\b"));
        }
    }

    #[test]
    fn canonical_paths_have_no_verbatim_prefix() {
        let c = canonical(&std::env::temp_dir()).unwrap();
        assert!(!c.to_string_lossy().starts_with(r"\\?\"), "{c:?}");
        assert!(!tex(&c).contains('\\'), "{}", tex(&c));
    }
}
