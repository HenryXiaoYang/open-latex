//! Locating the TeX Live binaries and preparing the environment our engine processes run in.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct TexLive {
    pub bin_dir: Option<PathBuf>,
    pub lualatex: PathBuf,
    /// Repository `tex/` directory holding rtex's Lua and LaTeX files.
    pub rtex_texdir: PathBuf,
}

/// Find the directory containing rtex's `tex/` support files: `$RTEX_TEXDIR`, then next to the
/// running executable as the release archives lay it out (`<prefix>/bin/rtex` with
/// `<prefix>/share/rtex/tex`, or a `tex/` beside or above the executable), then the source tree
/// this crate was built from.
pub fn find_rtex_texdir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("RTEX_TEXDIR") {
        return Ok(PathBuf::from(p));
    }
    let is_texdir = |d: &Path| d.join("rtex-dl.lua").is_file();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| crate::paths::canonical(&e).ok())
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        for c in [
            dir.join("../share/rtex/tex"),
            dir.join("tex"),
            dir.join("../tex"),
        ] {
            if is_texdir(&c) {
                return Ok(crate::paths::canonical(&c)?);
            }
        }
    }
    // Compile-time location of this crate → <repo>/tex
    let candidate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tex");
    if is_texdir(&candidate) {
        return Ok(crate::paths::canonical(&candidate)?);
    }
    bail!("cannot locate rtex's tex/ support files; set RTEX_TEXDIR")
}

/// Find `lualatex`: in `$RTEX_TEXLIVE_BIN` when set, else on `PATH`. Returns the directory from
/// `$RTEX_TEXLIVE_BIN` (if any) and the executable.
pub fn find_lualatex() -> Result<(Option<PathBuf>, PathBuf)> {
    let bin_dir = std::env::var_os("RTEX_TEXLIVE_BIN").map(PathBuf::from);
    let lualatex = match &bin_dir {
        Some(d) => d.join(crate::paths::exe("lualatex")),
        None => which("lualatex").context(
            "lualatex not found on PATH (source build/texlive.env or set RTEX_TEXLIVE_BIN)",
        )?,
    };
    if !lualatex.exists() {
        bail!("lualatex not found at {}", lualatex.display());
    }
    Ok((bin_dir, lualatex))
}

impl TexLive {
    pub fn discover() -> Result<TexLive> {
        let (bin_dir, lualatex) = find_lualatex()?;
        Ok(TexLive {
            bin_dir,
            lualatex,
            rtex_texdir: find_rtex_texdir()?,
        })
    }

    /// A `Command` for lualatex with the environment rtex needs: our tex/ dirs on the search
    /// paths, unrestricted file writing (we control the inputs), untruncated log lines.
    pub fn lualatex_cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new(&self.lualatex);
        c.current_dir(cwd);
        use crate::paths::{prepend_search, tex};
        let texinputs = prepend_search(
            &format!("{}//", tex(&self.rtex_texdir.join("latex"))),
            "TEXINPUTS",
        );
        let luainputs = prepend_search(&format!("{}//", tex(&self.rtex_texdir)), "LUAINPUTS");
        c.env("TEXINPUTS", texinputs)
            .env("LUAINPUTS", luainputs)
            .env("openout_any", "a")
            .env("max_print_line", "100000")
            .env("error_line", "254")
            .env("half_error_line", "238");
        if let Some(d) = &self.bin_dir {
            crate::paths::prepend_bin_dir(&mut c, d);
        }
        c
    }
}

fn which(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH unset")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(crate::paths::exe(name));
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("{name} not found")
}
