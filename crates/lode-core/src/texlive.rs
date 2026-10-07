//! Locating the TeX Live binaries and preparing the environment our engine processes run in.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct TexLive {
    pub bin_dir: Option<PathBuf>,
    pub lualatex: PathBuf,
    /// Repository `tex/` directory holding lode's Lua and LaTeX files.
    pub lode_texdir: PathBuf,
}

/// Find the directory containing lode's `tex/` support files.
pub fn find_lode_texdir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("LODE_TEXDIR") {
        return Ok(PathBuf::from(p));
    }
    // Compile-time location of this crate → <repo>/tex
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = here.join("../../tex");
    if candidate.join("lode-dl.lua").exists() {
        return Ok(candidate.canonicalize()?);
    }
    bail!("cannot locate lode tex/ directory; set LODE_TEXDIR")
}

impl TexLive {
    pub fn discover() -> Result<TexLive> {
        let bin_dir = std::env::var_os("LODE_TEXLIVE_BIN").map(PathBuf::from);
        let lualatex = match &bin_dir {
            Some(d) => d.join("lualatex"),
            None => which("lualatex").context("lualatex not found on PATH (source build/texlive.env or set LODE_TEXLIVE_BIN)")?,
        };
        if !lualatex.exists() {
            bail!("lualatex not found at {}", lualatex.display());
        }
        Ok(TexLive { bin_dir, lualatex, lode_texdir: find_lode_texdir()? })
    }

    /// A `Command` for lualatex with the environment lode needs: our tex/ dirs on the search
    /// paths, unrestricted file writing (we control the inputs), untruncated log lines.
    pub fn lualatex_cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new(&self.lualatex);
        c.current_dir(cwd);
        let sep = ":";
        let texinputs = format!("{}//{}{}", self.lode_texdir.join("latex").display(), sep, std::env::var("TEXINPUTS").unwrap_or_default());
        let luainputs = format!("{}//{}{}", self.lode_texdir.display(), sep, std::env::var("LUAINPUTS").unwrap_or_default());
        c.env("TEXINPUTS", texinputs)
            .env("LUAINPUTS", luainputs)
            .env("openout_any", "a")
            .env("max_print_line", "100000")
            .env("error_line", "254")
            .env("half_error_line", "238");
        if let Some(d) = &self.bin_dir {
            let path = format!("{}:{}", d.display(), std::env::var("PATH").unwrap_or_default());
            c.env("PATH", path);
        }
        c
    }
}

fn which(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH unset")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("{name} not found")
}
