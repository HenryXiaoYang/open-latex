//! Conservative fast-path eligibility: an **allow-list** over the paragraph source.
//!
//! A paragraph may take the per-keystroke path only when it can be typeset in isolation with
//! no effect on, and no dependence on, state outside the paragraph beyond what the captured
//! context replays. Anything not explicitly allowed — including every macro defined in the
//! preamble — sends the paragraph to the background path. See plan §3 / docs/ARCHITECTURE.md.

use std::collections::BTreeSet;

/// Why a paragraph is not fast-eligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Blank,
    UnbalancedBraces,
    UnbalancedMath,
    DisplayMath,
    EnvironmentBoundary,
    ParagraphBreak,
    Verbatim,
    DisallowedMacro(String),
    DisallowedMathMacro(String),
    SizeDeclarationOutsideGroup(String),
    EngineFlag(String),
    NoContext,
    NotTopLevel,
    NonDefaultEverypar,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::DisallowedMacro(m) => write!(f, "macro \\{m} is not on the fast-path allow-list"),
            Reason::DisallowedMathMacro(m) => write!(f, "math macro \\{m} is not on the fast-path allow-list"),
            Reason::SizeDeclarationOutsideGroup(m) => write!(f, "\\{m} outside a brace group would leak past the paragraph"),
            Reason::EngineFlag(s) => write!(f, "engine saw {s} in this paragraph"),
            other => write!(f, "{other:?}"),
        }
    }
}

const TEXT_MACROS: &[&str] = &[
    // font switches
    "emph", "textit", "textbf", "textsc", "textsf", "texttt", "textrm", "textmd", "textup", "textsl", "textnormal",
    "textsuperscript", "textsubscript",
    // spacing and breaks
    "\\", ",", ";", ":", "!", "quad", "qquad", "hspace", "hspace*", "mbox", "phantom", "hphantom", "vphantom", "noindent", "indent", "thinspace", "enspace", "enskip", "nolinebreak", "linebreak", "newline", "allowbreak", "-", "/", "@",
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
];

/// Declarations that are only allowed *inside* a brace group (they would otherwise leak).
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
    "ell", "hbar", "imath", "jmath", "aleph", "Re", "Im", "wp", "top", "bot", "dagger", "ddagger", "colon", "quad", "qquad", ",", ";", ":", "!", " ", "displaystyle", "textstyle", "scriptstyle", "scriptscriptstyle", "limits", "nolimits", "not", "phantom", "hphantom", "vphantom", "cdot", "label",
    "%", "$", "#", "&", "_",
];

/// Patterns that make a paragraph background-only regardless of allow-lists.
const HARD_STOPS: &[(&str, Reason)] = &[
    ("\\begin", Reason::EnvironmentBoundary),
    ("\\end", Reason::EnvironmentBoundary),
    ("\\[", Reason::DisplayMath),
    ("\\]", Reason::DisplayMath),
    ("$$", Reason::DisplayMath),
    ("\\par", Reason::ParagraphBreak),
    ("\\verb", Reason::Verbatim),
];

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Extra text-mode macros the host vouches for (pure, no global side effects).
    pub trusted_macros: BTreeSet<String>,
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

/// Lexical check of a paragraph's source. Returns the (possibly empty) list of reasons.
pub fn check_source(src: &str, policy: &Policy) -> Vec<Reason> {
    let text = strip_comments(src);
    let mut reasons = Vec::new();
    if text.trim().is_empty() {
        return vec![Reason::Blank];
    }
    // blank line inside = paragraph break
    let mut blank_inside = false;
    let lines: Vec<&str> = text.trim_end().lines().collect();
    for l in &lines[..lines.len().saturating_sub(1)] {
        if l.trim().is_empty() {
            blank_inside = true;
        }
    }
    if blank_inside {
        reasons.push(Reason::ParagraphBreak);
    }
    for (pat, r) in HARD_STOPS {
        if text.contains(pat) {
            // `\par` must be a whole control word
            if *pat == "\\par" {
                let b = text.as_bytes();
                let mut idx = 0;
                while let Some(p) = text[idx..].find("\\par") {
                    let end = idx + p + 4;
                    if end >= b.len() || !is_letter(b[end]) {
                        if !reasons.contains(r) {
                            reasons.push(r.clone());
                        }
                        break;
                    }
                    idx = end;
                }
            } else if !reasons.contains(r) {
                reasons.push(r.clone());
            }
        }
    }
    // tokenizer: control sequences, braces, math toggles
    let b = text.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut in_math = false;
    let mut math_depth_at_entry = 0;
    while i < b.len() {
        let c = b[i];
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
                i = j;
            } else if n == b'(' {
                if in_math {
                    reasons.push(Reason::UnbalancedMath);
                }
                in_math = true;
                math_depth_at_entry = depth;
                i += 2;
                continue;
            } else if n == b')' {
                if !in_math || depth != math_depth_at_entry {
                    reasons.push(Reason::UnbalancedMath);
                }
                in_math = false;
                i += 2;
                continue;
            } else {
                name = (n as char).to_string();
                i += 2;
            }
            let allowed = if in_math {
                MATH_MACROS.contains(&name.as_str()) || policy.trusted_macros.contains(&name)
            } else if GROUP_ONLY_DECLARATIONS.contains(&name.as_str()) {
                if depth == 0 {
                    reasons.push(Reason::SizeDeclarationOutsideGroup(name.clone()));
                }
                true
            } else {
                TEXT_MACROS.contains(&name.as_str()) || policy.trusted_macros.contains(&name)
            };
            if !allowed {
                let r = if in_math { Reason::DisallowedMathMacro(name) } else { Reason::DisallowedMacro(name) };
                if !reasons.contains(&r) {
                    reasons.push(r);
                }
            }
            continue;
        }
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    reasons.push(Reason::UnbalancedBraces);
                    depth = 0;
                }
            }
            b'$' => {
                if in_math {
                    if depth != math_depth_at_entry {
                        reasons.push(Reason::UnbalancedMath);
                    }
                    in_math = false;
                } else {
                    in_math = true;
                    math_depth_at_entry = depth;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 && !reasons.contains(&Reason::UnbalancedBraces) {
        reasons.push(Reason::UnbalancedBraces);
    }
    if in_math && !reasons.contains(&Reason::UnbalancedMath) {
        reasons.push(Reason::UnbalancedMath);
    }
    reasons
}

/// Engine-side check from capture flags (`pre_linebreak_filter` node scan) and paragraph metadata.
pub fn check_engine(groupcode: &str, nest: i64, everypar: &str, has_begin: bool, flags: &std::collections::BTreeMap<String, i64>) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if !(groupcode.is_empty() && nest == 1) {
        reasons.push(Reason::NotTopLevel);
    }
    // microtype sets \everypar{\leftprotrusion} in some contexts; the server replays it.
    if !(everypar.is_empty() || everypar == "\\leftprotrusion ") {
        reasons.push(Reason::NonDefaultEverypar);
    }
    if !has_begin {
        reasons.push(Reason::NoContext);
    }
    for (k, v) in flags {
        if *v == 0 {
            continue;
        }
        // nested boxes and rules (math fractions, radicals, \mbox) are fine: the traversal
        // reproduces them; inserts, marks, adjust material and direction changes are not.
        let bad = matches!(k.as_str(), "ins" | "mark" | "adjust" | "dir")
            || (k.starts_with("whatsit_") && !matches!(k.as_str(), "whatsit_pdf_colorstack" | "whatsit_user_defined" | "whatsit_late_lua" | "whatsit_write"));
        if bad {
            reasons.push(Reason::EngineFlag(format!("{k}={v}")));
        }
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ok(s: &str) -> bool {
        check_source(s, &Policy::default()).is_empty()
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
        assert_eq!(check_source("See \\ref{sec:x}.", &Policy::default()), vec![Reason::DisallowedMacro("ref".into())]);
        assert!(check_source("Footnote\\footnote{x} here.", &Policy::default()).contains(&Reason::DisallowedMacro("footnote".into())));
        assert!(check_source("\\mymacro{x}", &Policy::default()).contains(&Reason::DisallowedMacro("mymacro".into())));
        assert!(check_source("Text \\large leaking.", &Policy::default()).contains(&Reason::SizeDeclarationOutsideGroup("large".into())));
        assert!(check_source("\\begin{itemize}\\item x\\end{itemize}", &Policy::default()).contains(&Reason::EnvironmentBoundary));
        assert!(check_source("a \\[ x \\] b", &Policy::default()).contains(&Reason::DisplayMath));
        assert!(check_source("one\n\ntwo", &Policy::default()).contains(&Reason::ParagraphBreak));
        assert!(check_source("a \\par b", &Policy::default()).contains(&Reason::ParagraphBreak));
        assert!(ok("a \\parbox-like? no: \\parskip is disallowed") == false);
    }
    #[test]
    fn balance() {
        assert!(check_source("Unbalanced { brace", &Policy::default()).contains(&Reason::UnbalancedBraces));
        assert!(check_source("Unbalanced } brace", &Policy::default()).contains(&Reason::UnbalancedBraces));
        assert!(check_source("Unbalanced $ math", &Policy::default()).contains(&Reason::UnbalancedMath));
        assert!(check_source("$a { b $ c }", &Policy::default()).contains(&Reason::UnbalancedMath));
        assert!(ok("Escaped \\{ \\} \\$ are fine."));
    }
    #[test]
    fn math_vocabulary() {
        assert!(check_source("$\\mycmd{x}$", &Policy::default()).contains(&Reason::DisallowedMathMacro("mycmd".into())));
        assert!(ok("$\\sum_{i=1}^{n} \\mathbf{v}_i \\cdot \\hat{w}$"));
    }
    #[test]
    fn trusted() {
        let mut p = Policy::default();
        p.trusted_macros.insert("mymacro".into());
        assert!(check_source("\\mymacro{x}", &p).is_empty());
    }
    #[test]
    fn comments_stripped() {
        assert!(ok("Text % \\ref{x} in a comment\nmore text"));
        assert!(!ok("Text \\% \\ref{x} not a comment"));
    }
    #[test]
    fn engine_flags() {
        let mut flags = std::collections::BTreeMap::new();
        flags.insert("glyphs".to_string(), 10);
        flags.insert("math".to_string(), 2);
        assert!(check_engine("", 1, "", true, &flags).is_empty());
        flags.insert("ins".to_string(), 1);
        assert_eq!(check_engine("", 1, "", true, &flags), vec![Reason::EngineFlag("ins=1".into())]);
        assert!(check_engine("vbox", 2, "", true, &BTreeMap::new()).contains(&Reason::NotTopLevel));
        assert!(check_engine("", 1, "\\@itemlabel", true, &BTreeMap::new()).contains(&Reason::NonDefaultEverypar));
    }
    use std::collections::BTreeMap;
}
