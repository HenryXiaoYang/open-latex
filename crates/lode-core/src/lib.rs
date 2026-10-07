//! lode-core: persistent LuaTeX paragraph server, background compiler, versioned layout store.

pub mod capture;
pub mod engine;
pub mod texlive;

/// Extract the preamble (everything before `\begin{document}`) from a LaTeX main file.
pub fn split_preamble(main_tex: &str) -> Option<(&str, &str)> {
    let idx = main_tex.find("\\begin{document}")?;
    Some((&main_tex[..idx], &main_tex[idx..]))
}
