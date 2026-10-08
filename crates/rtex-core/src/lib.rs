//! rtex-core: persistent LuaTeX paragraph server, background compiler, versioned layout store.

pub mod background;
pub mod capture;
pub mod document;
pub mod eligibility;
pub mod engine;
pub mod ffi;
pub mod fixtures;
pub mod layout;
pub mod session;
pub mod texlive;

pub use document::{Edit, ParaId, Revision};
pub use session::{Convergence, Event, Session, SessionConfig, Versions};

/// Extract the preamble (everything before `\begin{document}`) from a LaTeX main file.
pub fn split_preamble(main_tex: &str) -> Option<(&str, &str)> {
    let idx = main_tex.find("\\begin{document}")?;
    Some((&main_tex[..idx], &main_tex[idx..]))
}

/// The preamble of `project/main` with `\input`ted preamble files inlined (what `Policy` scans
/// and the fast server loads).
pub fn project_preamble(project: &std::path::Path, main: &str) -> anyhow::Result<String> {
    let files = document::load_project_files(project, main)?;
    let (pre, _) = split_preamble(&files[main])
        .ok_or_else(|| anyhow::anyhow!("no \\begin{{document}} in {main}"))?;
    Ok(document::expand_inputs(pre, &files))
}
