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

/// The preamble the fast server loads and `Policy` scans: `main`'s preamble with `\input`ted
/// files inlined, followed by the setup statements found in the document body (macros defined
/// after `\begin{document}`, `\renewcommand{\arraystretch}` …; see
/// `eligibility::setup_statements`).
pub fn server_preamble(texts: &std::collections::BTreeMap<String, String>, main: &str) -> String {
    let mut pre = texts
        .get(main)
        .and_then(|t| split_preamble(t))
        .map(|(p, _)| document::expand_inputs(p, texts))
        .unwrap_or_default();
    let setup = document::body_setup_statements(texts, main);
    if !setup.is_empty() {
        pre.push_str("\n% rtex: document setup found after \\begin{document}\n");
        for st in setup {
            pre.push_str(&st);
            pre.push('\n');
        }
    }
    pre
}

/// `server_preamble` of `project/main` read from disk (offline tools).
pub fn project_preamble(project: &std::path::Path, main: &str) -> anyhow::Result<String> {
    let files = document::load_project_files(project, main)?;
    split_preamble(&files[main])
        .ok_or_else(|| anyhow::anyhow!("no \\begin{{document}} in {main}"))?;
    Ok(server_preamble(&files, main))
}
