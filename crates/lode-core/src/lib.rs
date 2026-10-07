//! lode-core: persistent LuaTeX paragraph server, background compiler, versioned layout store.

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
