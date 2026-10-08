//! Conservative fast-path eligibility: an **allow-list** over a unit's source.
//!
//! A unit (paragraph, block environment or heading) may take the per-keystroke path only when
//! it can be typeset in isolation with no effect on, and no dependence on, state outside the
//! unit beyond what the captured context replays (parameters, fonts, counters, labels). Anything
//! not explicitly allowed — including every macro defined in the preamble — sends the unit to the
//! background path. See docs/ARCHITECTURE.md and docs/LIMITATIONS.md.

use std::collections::BTreeSet;

/// Why a unit is not fast-eligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Blank,
    UnbalancedBraces,
    UnbalancedMath,
    UnbalancedEnvironment,
    DisplayMath,
    EnvironmentBoundary,
    /// Text continues after a block environment inside the same span.
    TextAfterEnvironment,
    DisallowedEnvironment(String),
    ParagraphBreak,
    Verbatim,
    DisallowedMacro(String),
    DisallowedMathMacro(String),
    /// A macro that is only meaningful inside a particular environment (`\item`, `\caption`, `\hline`).
    MacroOutsideContext(String),
    /// A package the construct needs is not loaded (amsmath, graphicx, booktabs).
    NeedsPackage(String),
    SizeDeclarationOutsideGroup(String),
    EngineFlag(String),
    NoContext,
    NoPlacement,
    NotTopLevel,
    NonDefaultEverypar,
    /// The unit's source shape (paragraph / environment / heading) does not match the layout's unit.
    KindMismatch,
    /// The last fast compile of this unit exceeded the session's budget (ms).
    OverBudget(u64),
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::DisallowedMacro(m) => write!(f, "macro \\{m} is not on the fast-path allow-list"),
            Reason::DisallowedMathMacro(m) => write!(f, "math macro \\{m} is not on the fast-path allow-list"),
            Reason::DisallowedEnvironment(e) => write!(f, "environment {e} is not on the fast-path allow-list"),
            Reason::MacroOutsideContext(m) => write!(f, "\\{m} outside the environment it belongs to"),
            Reason::NeedsPackage(p) => write!(f, "needs package {p}, which the preamble does not load"),
            Reason::SizeDeclarationOutsideGroup(m) => write!(f, "\\{m} outside a brace group would leak past the paragraph"),
            Reason::EngineFlag(s) => write!(f, "engine saw {s} in this unit"),
            Reason::OverBudget(ms) => write!(f, "last fast compile took {ms} ms, over the budget"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// The shape of a unit's source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitShape {
    Par,
    Env(String),
    Heading(String),
}

const TEXT_MACROS: &[&str] = &[
    // font switches
    "emph", "textit", "textbf", "textsc", "textsf", "texttt", "textrm", "textmd", "textup", "textsl", "textnormal",
    "textsuperscript", "textsubscript",
    // spacing and breaks
    "\\", ",", ";", ":", "!", "quad", "qquad", "hspace", "hspace*", "mbox", "phantom", "hphantom", "vphantom", "noindent", "indent", "thinspace", "enspace", "enskip", "nolinebreak", "linebreak", "newline", "allowbreak", "-", "/", "@",
    "hfill", "hfil", "hss", "vspace", "vspace*", "smallskip", "medskip", "bigskip", "strut", "relax",
    // color (xcolor)
    "textcolor", "color",
    // characters, accents, symbols
    "'", "`", "^", "\"", "~", "=", ".", "u", "v", "H", "t", "c", "d", "b", "r", "k",
    "ss", "SS", "ae", "AE", "oe", "OE", "o", "O", "l", "L", "aa", "AA", "i", "j", "dh", "DH", "th", "TH", "ng", "NG",
    "&", "%", "$", "#", "_", "{", "}", "ldots", "dots", "textellipsis", "LaTeX", "TeX", "LaTeXe",
    "textendash", "textemdash", "textquoteleft", "textquoteright", "textquotedblleft", "textquotedblright",
    "textregistered", "copyright", "texttrademark", "textbackslash", "textbar", "textless", "textgreater",
    "textasciitilde", "textasciicircum", "textbullet", "textdagger", "textdaggerdbl", "textsection", "textparagraph",
    "textdegree", "textperiodcentered", "textvisiblespace", "textunderscore", "textbraceleft", "textbraceright",
    "textdollar", "textsterling", "texteuro", "textminus", "textasteriskcentered", "slash", "textcompwordmark",
    "nobreakspace", "ensuremath", "lowercase", "uppercase", "MakeUppercase", "MakeLowercase",
    "today",
    // cross-references, notes, index (replayed from the last pass's aux)
    "ref", "pageref", "label", "footnote", "footnotemark", "footnotetext", "index", "nocite",
    // lengths usable inside arguments
    "textwidth", "linewidth", "columnwidth", "textheight", "baselineskip", "parindent", "paperwidth", "paperheight",
    // alignment declarations (local to the environment they appear in)
    "centering", "raggedright", "raggedleft",
];

/// Declarations that are only allowed *inside* a brace group or an environment (they would
/// otherwise leak).
const GROUP_ONLY_DECLARATIONS: &[&str] = &[
    "itshape", "bfseries", "scshape", "sffamily", "ttfamily", "rmfamily", "mdseries", "upshape", "slshape", "normalfont", "em",
    "tiny", "scriptsize", "footnotesize", "small", "normalsize", "large", "Large", "LARGE", "huge", "Huge",
];

const MATH_MACROS: &[&str] = &[
    "frac", "dfrac", "tfrac", "sqrt", "sum", "prod", "int", "iint", "oint", "lim", "limsup", "liminf", "infty", "cdot", "cdots", "ldots", "dots", "vdots", "ddots",
    "times", "div", "pm", "mp", "leq", "le", "geq", "ge", "neq", "ne", "approx", "equiv", "sim", "simeq", "cong", "propto", "to", "rightarrow", "leftarrow", "Rightarrow", "Leftarrow", "leftrightarrow", "Leftrightarrow", "mapsto", "longrightarrow",
    "in", "notin", "ni", "subset", "subseteq", "supset", "supseteq", "cup", "cap", "setminus", "emptyset", "varnothing", "forall", "exists", "nexists", "neg", "lnot", "land", "lor", "wedge", "vee",
    "partial", "nabla", "prime", "angle", "perp", "parallel", "mid", "circ", "bullet", "star", "ast", "oplus", "otimes", "odot",
    "mathrm", "mathbf", "mathit", "mathsf", "mathtt", "mathcal", "mathbb", "mathfrak", "mathscr", "boldsymbol", "operatorname", "text", "textrm", "textit", "textbf", "mbox",
    "left", "right", "bigl", "bigr", "Bigl", "Bigr", "biggl", "biggr", "Biggl", "Biggr", "big", "Big", "bigg", "Bigg", "langle", "rangle", "lfloor", "rfloor", "lceil", "rceil", "vert", "Vert", "|", "{", "}", "lbrace", "rbrace", "backslash",
    "hat", "bar", "vec", "tilde", "dot", "ddot", "overline", "underline", "widehat", "widetilde", "overrightarrow", "overleftarrow", "acute", "grave", "breve", "check",
    "binom", "dbinom", "tbinom", "choose", "over", "atop", "stackrel", "overset", "underset", "substack",
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh", "log", "ln", "lg", "exp", "min", "max", "sup", "inf", "arg", "det", "dim", "deg", "gcd", "hom", "ker", "Pr",
    "alpha", "beta", "gamma", "delta", "epsilon", "varepsilon", "zeta", "eta", "theta", "vartheta", "iota", "kappa", "lambda", "mu", "nu", "xi", "pi", "varpi", "rho", "varrho", "sigma", "varsigma", "tau", "upsilon", "phi", "varphi", "chi", "psi", "omega",
    "Gamma", "Delta", "Theta", "Lambda", "Xi", "Pi", "Sigma", "Upsilon", "Phi", "Psi", "Omega",
    "ell", "hbar", "imath", "jmath", "aleph", "Re", "Im", "wp", "top", "bot", "dagger", "ddagger", "colon", "quad", "qquad", ",", ";", ":", "!", " ", "displaystyle", "textstyle", "scriptstyle", "scriptscriptstyle", "limits", "nolimits", "not", "phantom", "hphantom", "vphantom", "label",
    "%", "$", "#", "&", "_", "\\", "nonumber", "notag", "tag", "tag*", "intertext", "eqref", "ref", "qquad", "hspace", "hfill", "quad",
    "left.", "right.", "mathop", "mathbin", "mathrel", "mathord", "mathpunct", "mathinner", "mathopen", "mathclose", "allowbreak",
    "textcolor", "color",
];

/// Patterns that make a unit background-only regardless of allow-lists.
const HARD_STOPS: &[(&str, Reason)] = &[
    ("\\par", Reason::ParagraphBreak),
    ("\\verb", Reason::Verbatim),
];

/// Block environments that form units (or end a paragraph unit) and may nest.
pub const BLOCK_ENVS: &[&str] = &["itemize", "enumerate", "description", "quote", "quotation", "verse", "center", "flushleft", "flushright", "abstract"];
pub const FLOAT_ENVS: &[&str] = &["figure", "figure*", "table", "table*"];
/// Display-math environments (inside paragraph units).
pub const DISPLAY_ENVS: &[&str] = &["equation", "equation*", "displaymath", "eqnarray", "eqnarray*"];
pub const AMSMATH_DISPLAY_ENVS: &[&str] = &["align", "align*", "gather", "gather*", "multline", "multline*", "flalign", "flalign*", "alignat", "alignat*"];
/// Environments allowed inside units (not units themselves).
const INNER_ENVS: &[&str] = &["tabular", "tabular*", "minipage", "math", "em", "small", "footnotesize", "scriptsize", "large", "Large", "bfseries", "itshape", "sloppypar"];
const AMSMATH_INNER_ENVS: &[&str] = &["split", "aligned", "gathered", "cases", "pmatrix", "bmatrix", "vmatrix", "Vmatrix", "matrix", "smallmatrix", "array", "subarray"];
const LIST_ENVS: &[&str] = &["itemize", "enumerate", "description"];
const TABULAR_MACROS: &[&str] = &["hline", "cline", "multicolumn", "multirow"];
const BOOKTABS_MACROS: &[&str] = &["toprule", "midrule", "bottomrule", "cmidrule", "addlinespace", "specialrule"];
const FLOAT_MACROS: &[&str] = &["caption", "caption*"];
const HEADINGS: &[&str] = &["part", "chapter", "section", "subsection", "subsubsection", "paragraph", "subparagraph"];

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Extra text-mode macros the host vouches for (pure, no global side effects).
    pub trusted_macros: BTreeSet<String>,
    /// Theorem-like environments (from `\newtheorem` in the preamble or the host) treated as units.
    pub theorem_envs: BTreeSet<String>,
    pub amsmath: bool,
    pub graphicx: bool,
    pub booktabs: bool,
    /// `\cite` is replayable from the aux (plain `\bibcite`); false with biblatex/natbib.
    pub cite_ok: bool,
}

impl Policy {
    /// Derive package facts and theorem environments from the document preamble.
    pub fn from_preamble(preamble: &str, trusted: &[String], unit_envs: &[String]) -> Policy {
        let text = strip_comments(preamble);
        let has_pkg = |name: &str| -> bool {
            let mut idx = 0;
            while let Some(p) = text[idx..].find("\\usepackage") {
                let s = idx + p;
                let rest = &text[s..];
                let end = rest.find('\n').map(|e| s + e).unwrap_or(text.len());
                // the braced list may follow an optional argument
                if let Some(bo) = text[s..end.max(s)].find('{') {
                    if let Some(bc) = text[s + bo..].find('}') {
                        if text[s + bo + 1..s + bo + bc].split(',').any(|n| n.trim() == name) {
                            return true;
                        }
                    }
                }
                idx = s + 11;
            }
            text.contains(&format!("\\RequirePackage{{{name}}}"))
        };
        let mut theorem_envs = BTreeSet::new();
        let mut idx = 0;
        while let Some(p) = text[idx..].find("\\newtheorem") {
            let s = idx + p + "\\newtheorem".len();
            let rest = &text[s..];
            let rest = rest.strip_prefix('*').unwrap_or(rest);
            if let Some(b) = rest.strip_prefix('{') {
                if let Some(e) = b.find('}') {
                    theorem_envs.insert(b[..e].trim().to_string());
                }
            }
            idx = s;
        }
        for e in unit_envs {
            theorem_envs.insert(e.clone());
        }
        Policy {
            trusted_macros: trusted.iter().cloned().collect(),
            theorem_envs,
            amsmath: has_pkg("amsmath") || has_pkg("mathtools"),
            graphicx: has_pkg("graphicx") || has_pkg("graphics"),
            booktabs: has_pkg("booktabs"),
            cite_ok: !(has_pkg("biblatex") || has_pkg("natbib")),
        }
    }

    pub fn is_block_env(&self, name: &str) -> bool {
        BLOCK_ENVS.contains(&name) || FLOAT_ENVS.contains(&name) || self.theorem_envs.contains(name)
    }
    pub fn is_display_env(&self, name: &str) -> bool {
        DISPLAY_ENVS.contains(&name) || AMSMATH_DISPLAY_ENVS.contains(&name)
    }
    /// Comma-separated extra unit environments for the capture run (`$RTEX_UNIT_ENVS`).
    pub fn unit_envs_env(&self) -> String {
        self.theorem_envs.iter().cloned().collect::<Vec<_>>().join(",")
    }
}

/// Strip `%` comments (respecting `\%`).
pub fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut cut = line.len();
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'%' {
                cut = i;
                break;
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

fn is_letter(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'@'
}

/// Parse `\begin{name}` / `\end{name}` at `text[i..]` (i at the backslash); returns (name, index after `}`).
fn env_name(text: &str, i: usize, what: &str) -> Option<(String, usize)> {
    let rest = &text[i..];
    let rest2 = rest.strip_prefix(what)?;
    let rest3 = rest2.trim_start();
    let body = rest3.strip_prefix('{')?;
    let e = body.find('}')?;
    let name = body[..e].trim().to_string();
    let consumed = rest.len() - body.len() + e + 1;
    Some((name, i + consumed))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Text,
    Math,
}

struct Frame {
    mode: Mode,
    env: Option<String>,
    depth_at_entry: i32,
}

/// Lexical check of a unit's source. Returns its shape and the (possibly empty) list of reasons.
pub fn classify_source(src: &str, policy: &Policy) -> (UnitShape, Vec<Reason>) {
    let text = strip_comments(src);
    let mut reasons: Vec<Reason> = Vec::new();
    let push = |reasons: &mut Vec<Reason>, r: Reason| {
        if !reasons.contains(&r) {
            reasons.push(r);
        }
    };
    if text.trim().is_empty() {
        return (UnitShape::Par, vec![Reason::Blank]);
    }
    let trimmed = text.trim();
    // shape: a block/float/theorem environment spanning the whole unit, a heading, or a paragraph
    let mut shape = UnitShape::Par;
    if let Some((name, _)) = env_name(trimmed, 0, "\\begin") {
        let end = format!("\\end{{{name}}}");
        if trimmed.ends_with(&end) && policy.is_block_env(&name) {
            shape = UnitShape::Env(name);
        }
    } else if trimmed.starts_with('\\') {
        let b = trimmed.as_bytes();
        let mut j = 1;
        while j < b.len() && is_letter(b[j]) {
            j += 1;
        }
        let name = &trimmed[1..j];
        if HEADINGS.contains(&name) {
            shape = UnitShape::Heading(name.to_string());
        }
    }
    for (pat, r) in HARD_STOPS {
        if let Some(mut idx) = text.find(pat) {
            // must be a whole control word
            let b = text.as_bytes();
            loop {
                let end = idx + pat.len();
                if end >= b.len() || !is_letter(b[end]) {
                    push(&mut reasons, r.clone());
                    break;
                }
                match text[end..].find(pat) {
                    Some(p) => idx = end + p,
                    None => break,
                }
            }
        }
    }
    let b = text.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut stack: Vec<Frame> = vec![Frame { mode: Mode::Text, env: None, depth_at_entry: 0 }];
    let mut env_depth = 0; // block environments open
    let mut block_env_closed_at: Option<usize> = None; // index after the last `\end{block}` at env_depth 0
    let mut text_group_pending = false; // next brace group is text mode (\text{...} in math)
    let mut line_start = 0usize;
    while i < b.len() {
        let c = b[i];
        // paragraph breaks (blank lines) are only fine inside block environments
        if c == b'\n' {
            let line = &text[line_start..i];
            if line.trim().is_empty() && i > 0 && env_depth == 0 && i + 1 < b.len() && text[i + 1..].trim().len() > 0 && line_start > 0 {
                push(&mut reasons, Reason::ParagraphBreak);
            }
            line_start = i + 1;
            i += 1;
            continue;
        }
        let in_math = stack.last().map(|f| f.mode == Mode::Math).unwrap_or(false);
        if c == b'\\' {
            if i + 1 >= b.len() {
                break;
            }
            let n = b[i + 1];
            let name: String;
            if is_letter(n) {
                let mut j = i + 1;
                while j < b.len() && is_letter(b[j]) {
                    j += 1;
                }
                name = text[i + 1..j].to_string();
                // `\tag*`, `\caption*`, `\hspace*`
                if j < b.len() && b[j] == b'*' && matches!(name.as_str(), "tag" | "caption" | "hspace" | "vspace" | "section" | "subsection" | "subsubsection" | "chapter" | "part" | "paragraph" | "subparagraph") {
                    i = j + 1;
                } else {
                    i = j;
                }
            } else if n == b'(' {
                if in_math {
                    push(&mut reasons, Reason::UnbalancedMath);
                }
                stack.push(Frame { mode: Mode::Math, env: None, depth_at_entry: depth });
                i += 2;
                continue;
            } else if n == b')' {
                if !in_math || stack.last().map(|f| f.depth_at_entry != depth).unwrap_or(true) {
                    push(&mut reasons, Reason::UnbalancedMath);
                }
                if in_math {
                    stack.pop();
                }
                i += 2;
                continue;
            } else if n == b'[' {
                if in_math {
                    push(&mut reasons, Reason::UnbalancedMath);
                }
                stack.push(Frame { mode: Mode::Math, env: None, depth_at_entry: depth });
                i += 2;
                continue;
            } else if n == b']' {
                if !in_math {
                    push(&mut reasons, Reason::UnbalancedMath);
                } else {
                    stack.pop();
                }
                i += 2;
                continue;
            } else {
                name = (n as char).to_string();
                i += 2;
            }
            // environments
            if name == "begin" {
                let Some((env, after)) = env_name(&text, i - 6, "\\begin") else {
                    push(&mut reasons, Reason::UnbalancedEnvironment);
                    continue;
                };
                i = after;
                if block_env_closed_at.is_some() && env_depth == 0 {
                    // a second block environment after one closed: still fine (both inside the unit)
                }
                let is_amsmath_display = AMSMATH_DISPLAY_ENVS.contains(&env.as_str());
                let is_amsmath_inner = AMSMATH_INNER_ENVS.contains(&env.as_str());
                if policy.is_display_env(&env) {
                    if is_amsmath_display && !policy.amsmath {
                        push(&mut reasons, Reason::NeedsPackage("amsmath".into()));
                    }
                    if in_math {
                        push(&mut reasons, Reason::UnbalancedMath);
                    }
                    stack.push(Frame { mode: Mode::Math, env: Some(env), depth_at_entry: depth });
                } else if policy.is_block_env(&env) {
                    if in_math {
                        push(&mut reasons, Reason::UnbalancedMath);
                    }
                    if block_env_closed_at.is_some() && env_depth == 0 && shape == UnitShape::Par {
                        // text (or another environment) after a block environment: the capture
                        // closes the paragraph unit at the first environment's end
                        push(&mut reasons, Reason::TextAfterEnvironment);
                    }
                    env_depth += 1;
                    stack.push(Frame { mode: Mode::Text, env: Some(env), depth_at_entry: depth });
                } else if INNER_ENVS.contains(&env.as_str()) || is_amsmath_inner {
                    if is_amsmath_inner && !policy.amsmath {
                        push(&mut reasons, Reason::NeedsPackage("amsmath".into()));
                    }
                    let mode = if is_amsmath_inner || env == "math" { Mode::Math } else { Mode::Text };
                    if env == "math" && in_math {
                        push(&mut reasons, Reason::UnbalancedMath);
                    }
                    stack.push(Frame { mode: if is_amsmath_inner { if in_math { Mode::Math } else { Mode::Math } } else { mode }, env: Some(env), depth_at_entry: depth });
                } else {
                    push(&mut reasons, Reason::DisallowedEnvironment(env.clone()));
                    stack.push(Frame { mode: Mode::Text, env: Some(env), depth_at_entry: depth });
                }
                continue;
            }
            if name == "end" {
                let Some((env, after)) = env_name(&text, i - 4, "\\end") else {
                    push(&mut reasons, Reason::UnbalancedEnvironment);
                    continue;
                };
                i = after;
                match stack.iter().rposition(|f| f.env.as_deref() == Some(env.as_str())) {
                    Some(pos) if pos == stack.len() - 1 => {
                        let f = stack.pop().unwrap();
                        if f.depth_at_entry != depth {
                            push(&mut reasons, Reason::UnbalancedBraces);
                            depth = f.depth_at_entry;
                        }
                        if policy.is_block_env(&env) {
                            env_depth -= 1;
                            if env_depth == 0 {
                                block_env_closed_at = Some(i);
                            }
                        }
                    }
                    _ => push(&mut reasons, Reason::UnbalancedEnvironment),
                }
                continue;
            }
            // macros
            let allowed = if in_math {
                let ok = MATH_MACROS.contains(&name.as_str()) || policy.trusted_macros.contains(&name);
                if ok && matches!(name.as_str(), "text" | "mbox" | "textrm" | "textit" | "textbf" | "intertext") {
                    text_group_pending = true;
                }
                if (name == "tag" || name == "notag" || name == "nonumber" || name == "intertext" || name == "eqref") && !policy.amsmath && name != "nonumber" {
                    push(&mut reasons, Reason::NeedsPackage("amsmath".into()));
                }
                ok
            } else if GROUP_ONLY_DECLARATIONS.contains(&name.as_str()) {
                if depth == 0 && env_depth == 0 && !matches!(shape, UnitShape::Heading(_)) {
                    push(&mut reasons, Reason::SizeDeclarationOutsideGroup(name.clone()));
                }
                true
            } else if HEADINGS.contains(&name.as_str()) {
                // only as the unit's own heading command
                matches!(&shape, UnitShape::Heading(h) if *h == name) && i <= name.len() + 2 + text.len() - trimmed.len()
            } else if name == "item" {
                let in_list = stack.iter().any(|f| f.env.as_deref().map(|e| LIST_ENVS.contains(&e)).unwrap_or(false));
                if !in_list {
                    push(&mut reasons, Reason::MacroOutsideContext(name.clone()));
                }
                true
            } else if FLOAT_MACROS.contains(&name.as_str()) {
                let in_float = stack.iter().any(|f| f.env.as_deref().map(|e| FLOAT_ENVS.contains(&e)).unwrap_or(false));
                if !in_float {
                    push(&mut reasons, Reason::MacroOutsideContext(name.clone()));
                }
                true
            } else if TABULAR_MACROS.contains(&name.as_str()) || BOOKTABS_MACROS.contains(&name.as_str()) {
                let in_tab = stack.iter().any(|f| f.env.as_deref().map(|e| e.starts_with("tabular") || e == "array").unwrap_or(false));
                if !in_tab {
                    push(&mut reasons, Reason::MacroOutsideContext(name.clone()));
                }
                if BOOKTABS_MACROS.contains(&name.as_str()) && !policy.booktabs {
                    push(&mut reasons, Reason::NeedsPackage("booktabs".into()));
                }
                true
            } else if name == "includegraphics" {
                if !policy.graphicx {
                    push(&mut reasons, Reason::NeedsPackage("graphicx".into()));
                }
                true
            } else if name == "cite" || name == "citep" || name == "citet" {
                if !policy.cite_ok || name != "cite" {
                    push(&mut reasons, Reason::DisallowedMacro(name.clone()));
                }
                true
            } else if name == "eqref" {
                if !policy.amsmath {
                    push(&mut reasons, Reason::NeedsPackage("amsmath".into()));
                }
                true
            } else {
                TEXT_MACROS.contains(&name.as_str()) || policy.trusted_macros.contains(&name)
            };
            if !allowed {
                let r = if in_math { Reason::DisallowedMathMacro(name) } else { Reason::DisallowedMacro(name) };
                push(&mut reasons, r);
            }
            continue;
        }
        match c {
            b'{' => {
                depth += 1;
                if text_group_pending {
                    text_group_pending = false;
                    stack.push(Frame { mode: Mode::Text, env: None, depth_at_entry: depth });
                }
            }
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    push(&mut reasons, Reason::UnbalancedBraces);
                    depth = 0;
                }
                // leaving a \text{...} group
                if let Some(f) = stack.last() {
                    if f.env.is_none() && f.mode == Mode::Text && stack.len() > 1 && f.depth_at_entry == depth + 1 {
                        stack.pop();
                    }
                }
            }
            b'$' => {
                let double = i + 1 < b.len() && b[i + 1] == b'$';
                if double {
                    i += 1;
                }
                if in_math {
                    let f = stack.last().unwrap();
                    if f.env.is_some() || f.depth_at_entry != depth {
                        push(&mut reasons, Reason::UnbalancedMath);
                    } else {
                        stack.pop();
                    }
                } else {
                    stack.push(Frame { mode: Mode::Math, env: None, depth_at_entry: depth });
                }
            }
            _ => {
                if block_env_closed_at.is_some() && env_depth == 0 && shape == UnitShape::Par && !c.is_ascii_whitespace() {
                    push(&mut reasons, Reason::TextAfterEnvironment);
                }
            }
        }
        i += 1;
    }
    if depth != 0 {
        push(&mut reasons, Reason::UnbalancedBraces);
    }
    if stack.len() > 1 {
        let f = stack.last().unwrap();
        if f.env.is_some() {
            push(&mut reasons, Reason::UnbalancedEnvironment);
        } else if f.mode == Mode::Math {
            push(&mut reasons, Reason::UnbalancedMath);
        }
    }
    let _ = Reason::EnvironmentBoundary;
    let _ = Reason::DisplayMath;
    (shape, reasons)
}

/// Lexical check of a unit's source. Returns the (possibly empty) list of reasons.
pub fn check_source(src: &str, policy: &Policy) -> Vec<Reason> {
    classify_source(src, policy).1
}

/// `\everypar` contents the server may replay verbatim at a unit's start: the kernel's own
/// patterns (after a heading, after an environment, first paragraph of a box) and microtype's
/// `\leftprotrusion`. Everything else makes the unit background-only.
pub fn everypar_allowed(ep: &str) -> bool {
    let e = ep.trim_start_matches("\\leftprotrusion ").trim_end_matches("\\leftprotrusion ");
    if e.is_empty() {
        return true;
    }
    const PATTERNS: &[&str] = &[
        // \@afterheading (with and without the \if@afterindent branch)
        "\\if@nobreak \\@nobreakfalse \\clubpenalty \\@M \\if@afterindent \\else {\\setbox \\z@ \\lastbox }\\fi \\else \\clubpenalty \\@clubpenalty \\everypar {}\\fi ",
        "\\if@nobreak \\@nobreakfalse \\clubpenalty \\@M \\setbox \\z@ \\lastbox \\else \\clubpenalty \\@clubpenalty \\everypar {}\\fi ",
        // \@doendpe (first paragraph after an environment)
        "{\\setbox \\z@ \\lastbox }\\everypar {}\\@endpefalse ",
        // \@setminipage (first paragraph in a float or minipage)
        "\\@minipagefalse \\everypar {}",
    ];
    PATTERNS.iter().any(|p| *p == e)
}

/// Engine-side check of a unit from the capture: `everypar` pattern, node flags, members.
pub fn check_engine_unit(kind: &str, everypar: &str, has_context: bool, rows: i64, flags: &std::collections::BTreeMap<String, i64>) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if !everypar_allowed(everypar) {
        reasons.push(Reason::NonDefaultEverypar);
    }
    if !has_context {
        reasons.push(Reason::NoContext);
    }
    if rows == 0 {
        reasons.push(Reason::NoPlacement);
    }
    let _ = kind;
    for (k, v) in flags {
        if *v == 0 {
            continue;
        }
        // inserts (footnotes), marks (running heads) and adjust material are placed by the page
        // builder and refreshed by the next layout; direction changes are not supported.
        if k == "dir" {
            reasons.push(Reason::EngineFlag(format!("{k}={v}")));
        }
    }
    reasons
}

/// Legacy per-paragraph engine check (kept for the verification tools).
pub fn check_engine(groupcode: &str, nest: i64, everypar: &str, has_begin: bool, flags: &std::collections::BTreeMap<String, i64>) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if !(groupcode.is_empty() && nest == 1) {
        reasons.push(Reason::NotTopLevel);
    }
    if !everypar_allowed(everypar) {
        reasons.push(Reason::NonDefaultEverypar);
    }
    if !has_begin {
        reasons.push(Reason::NoContext);
    }
    for (k, v) in flags {
        if *v != 0 && k == "dir" {
            reasons.push(Reason::EngineFlag(format!("{k}={v}")));
        }
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pol() -> Policy {
        Policy { amsmath: true, graphicx: true, booktabs: true, cite_ok: true, ..Default::default() }
    }
    fn ok(s: &str) -> bool {
        check_source(s, &pol()).is_empty()
    }
    fn reasons(s: &str) -> Vec<Reason> {
        check_source(s, &pol())
    }
    #[test]
    fn plain_and_fonts_ok() {
        assert!(ok("Plain text, with \\emph{emphasis} and \\textbf{bold}, ``quotes'' and a dash---here."));
        assert!(ok("Inline $x^2 + \\frac{1}{2} \\leq \\alpha$ math and \\(a_i\\) too.\nSecond line."));
        assert!(ok("{\\itshape grouped declaration} and {\\small small}."));
        assert!(ok("A \\textcolor{red}{red} word and \\'e accents, 50\\% and \\&."));
    }
    #[test]
    fn disallowed() {
        assert!(reasons("Footnote\\footnote{x} here.").is_empty());
        assert!(reasons("\\mymacro{x}").contains(&Reason::DisallowedMacro("mymacro".into())));
        assert!(reasons("Text \\large leaking.").contains(&Reason::SizeDeclarationOutsideGroup("large".into())));
        assert!(reasons("one\n\ntwo").contains(&Reason::ParagraphBreak));
        assert!(reasons("a \\par b").contains(&Reason::ParagraphBreak));
        assert!(!ok("a \\parbox-like? no: \\parskip is disallowed"));
        assert!(reasons("\\begin{tikzpicture}\\end{tikzpicture}").contains(&Reason::DisallowedEnvironment("tikzpicture".into())));
    }
    #[test]
    fn balance() {
        assert!(reasons("Unbalanced { brace").contains(&Reason::UnbalancedBraces));
        assert!(reasons("Unbalanced } brace").contains(&Reason::UnbalancedBraces));
        assert!(reasons("Unbalanced $ math").contains(&Reason::UnbalancedMath));
        assert!(reasons("$a { b $ c }").contains(&Reason::UnbalancedMath));
        assert!(ok("Escaped \\{ \\} \\$ are fine."));
        assert!(reasons("\\begin{itemize}\\item x").contains(&Reason::UnbalancedEnvironment));
    }
    #[test]
    fn math_vocabulary() {
        assert!(reasons("$\\mycmd{x}$").contains(&Reason::DisallowedMathMacro("mycmd".into())));
        assert!(ok("$\\sum_{i=1}^{n} \\mathbf{v}_i \\cdot \\hat{w}$"));
        assert!(ok("$\\text{for all } x$ and $\\mbox{the \\emph{text}} y$"));
    }
    #[test]
    fn display_math_and_refs() {
        let (shape, r) = classify_source("Text before\n\\[ x = y \\]\ntext after with \\eqref{eq:a} and\n\\begin{equation} a = b \\label{eq:a} \\end{equation}\nand \\begin{align} a &= b \\\\ c &= d \\end{align} done.", &pol());
        assert_eq!(shape, UnitShape::Par);
        assert!(r.is_empty(), "{r:?}");
        assert!(ok("See Section~\\ref{sec:x} on page~\\pageref{sec:x} and \\cite{knuth}."));
        let mut p = pol();
        p.amsmath = false;
        assert!(check_source("\\begin{align} a &= b \\end{align}", &p).contains(&Reason::NeedsPackage("amsmath".into())));
        p.cite_ok = false;
        assert!(check_source("See \\cite{k}.", &p).contains(&Reason::DisallowedMacro("cite".into())));
    }
    #[test]
    fn environments() {
        let (shape, r) = classify_source("\\begin{itemize}\n\\item First\n\\item Second with $x$\n\\end{itemize}", &pol());
        assert_eq!(shape, UnitShape::Env("itemize".into()));
        assert!(r.is_empty(), "{r:?}");
        let (shape, r) = classify_source("\\begin{figure}[tbp]\\centering\\includegraphics[width=0.5\\textwidth]{figure.png}\\caption{Figure 1.}\\label{fig:1}\\end{figure}", &pol());
        assert_eq!(shape, UnitShape::Env("figure".into()));
        assert!(r.is_empty(), "{r:?}");
        let (shape, r) = classify_source("\\begin{table}[h]\\centering\\begin{tabular}{lr}\\toprule a & b \\\\ \\midrule 1 & 2 \\\\ \\bottomrule\\end{tabular}\\caption{T}\\end{table}", &pol());
        assert_eq!(shape, UnitShape::Env("table".into()));
        assert!(r.is_empty(), "{r:?}");
        assert!(reasons("\\item outside").contains(&Reason::MacroOutsideContext("item".into())));
        assert!(reasons("\\caption{x}").contains(&Reason::MacroOutsideContext("caption".into())));
        // a paragraph ending in a list is one unit; text after the list is not
        assert!(ok("Intro text:\n\\begin{itemize}\\item a\\end{itemize}"));
        assert!(reasons("Intro:\n\\begin{itemize}\\item a\\end{itemize}\nmore text").contains(&Reason::TextAfterEnvironment));
        // blank lines inside a block environment are fine
        assert!(ok("\\begin{quote}\nfirst\n\nsecond\n\\end{quote}"));
        let mut p = pol();
        p.theorem_envs.insert("theorem".into());
        assert_eq!(classify_source("\\begin{theorem}\\label{t}Let $x$.\\end{theorem}", &p).0, UnitShape::Env("theorem".into()));
    }
    #[test]
    fn headings() {
        let (shape, r) = classify_source("\\section{A \\emph{title}}\\label{sec:a}", &pol());
        assert_eq!(shape, UnitShape::Heading("section".into()));
        assert!(r.is_empty(), "{r:?}");
        assert!(reasons("Text \\section{x}").contains(&Reason::DisallowedMacro("section".into())));
    }
    #[test]
    fn trusted() {
        let mut p = pol();
        p.trusted_macros.insert("mymacro".into());
        assert!(check_source("\\mymacro{x}", &p).is_empty());
    }
    #[test]
    fn comments_stripped() {
        assert!(ok("Text % \\mymacro{x} in a comment\nmore text"));
        assert!(!ok("Text \\% \\mymacro{x} not a comment"));
    }
    #[test]
    fn preamble_policy() {
        let p = Policy::from_preamble("\\documentclass{book}\n\\usepackage{amsmath,graphicx}\n\\usepackage[backend=biber]{biblatex}\n\\newtheorem{theorem}{Theorem}\n% \\usepackage{booktabs}\n", &[], &[]);
        assert!(p.amsmath && p.graphicx && !p.booktabs && !p.cite_ok);
        assert!(p.theorem_envs.contains("theorem"));
    }
    #[test]
    fn everypar_patterns() {
        assert!(everypar_allowed(""));
        assert!(everypar_allowed("\\leftprotrusion "));
        assert!(everypar_allowed("{\\setbox \\z@ \\lastbox }\\everypar {}\\@endpefalse "));
        assert!(!everypar_allowed("\\@itemlabel"));
        let mut flags = std::collections::BTreeMap::new();
        flags.insert("ins".to_string(), 1);
        assert!(check_engine_unit("par", "", true, 3, &flags).is_empty());
        flags.insert("dir".to_string(), 1);
        assert_eq!(check_engine_unit("par", "", true, 3, &flags), vec![Reason::EngineFlag("dir=1".into())]);
    }
}
