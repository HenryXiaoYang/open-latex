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
    /// A counter command before the unit's first text runs in vertical mode, before the point
    /// where the unit's counters are captured; replaying it would count twice.
    LeadingCounter(String),
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
            Reason::DisallowedMacro(m) => {
                write!(f, "macro \\{m} is not on the fast-path allow-list")
            }
            Reason::DisallowedMathMacro(m) => {
                write!(f, "math macro \\{m} is not on the fast-path allow-list")
            }
            Reason::DisallowedEnvironment(e) => {
                write!(f, "environment {e} is not on the fast-path allow-list")
            }
            Reason::MacroOutsideContext(m) => {
                write!(f, "\\{m} outside the environment it belongs to")
            }
            Reason::NeedsPackage(p) => {
                write!(f, "needs package {p}, which the preamble does not load")
            }
            Reason::SizeDeclarationOutsideGroup(m) => write!(
                f,
                "\\{m} outside a brace group would leak past the paragraph"
            ),
            Reason::EngineFlag(s) => write!(f, "engine saw {s} in this unit"),
            Reason::LeadingCounter(m) => write!(
                f,
                "\\{m} before the unit's text runs before its counters are captured; put it inside the paragraph or environment, or on its own line"
            ),
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
    "emph",
    "textit",
    "textbf",
    "textsc",
    "textsf",
    "texttt",
    "textrm",
    "textmd",
    "textup",
    "textsl",
    "textnormal",
    "textsuperscript",
    "textsubscript",
    // spacing and breaks
    "\\",
    ",",
    ";",
    ":",
    "!",
    "quad",
    "qquad",
    "hspace",
    "hspace*",
    "mbox",
    "phantom",
    "hphantom",
    "vphantom",
    "noindent",
    "indent",
    "thinspace",
    "enspace",
    "enskip",
    "nolinebreak",
    "linebreak",
    "newline",
    "allowbreak",
    "-",
    "/",
    "@",
    " ", // control space
    "hfill",
    "hfil",
    "hss",
    "vspace",
    "vspace*",
    "smallskip",
    "medskip",
    "bigskip",
    "strut",
    "relax",
    "par",
    "vfill",
    "vfil",
    "newpage",
    "clearpage",
    "pagebreak",
    "nopagebreak",
    "samepage",
    "thispagestyle",
    // boxes and rules (pure horizontal material)
    "parbox",
    "fbox",
    "framebox",
    "makebox",
    "raisebox",
    "colorbox",
    "fcolorbox",
    "underline",
    "rule",
    "hrulefill",
    "dotfill",
    // counters: the server restores the idle values after each compile and reports what the
    // unit advanced; the session schedules a pass when that changes (what follows renumbers)
    "setcounter",
    "addtocounter",
    "stepcounter",
    "refstepcounter",
    // counter formats (enumitem label specs, \alph{counter} …)
    "alph",
    "Alph",
    "arabic",
    "roman",
    "Roman",
    "fnsymbol",
    "arraybackslash",
    "tabularnewline",
    "checkmark",
    // color (xcolor)
    "textcolor",
    "color",
    // characters, accents, symbols
    "'",
    "`",
    "^",
    "\"",
    "~",
    "=",
    ".",
    "u",
    "v",
    "H",
    "t",
    "c",
    "d",
    "b",
    "r",
    "k",
    "ss",
    "SS",
    "ae",
    "AE",
    "oe",
    "OE",
    "o",
    "O",
    "l",
    "L",
    "aa",
    "AA",
    "i",
    "j",
    "dh",
    "DH",
    "th",
    "TH",
    "ng",
    "NG",
    "&",
    "%",
    "$",
    "#",
    "_",
    "{",
    "}",
    "ldots",
    "dots",
    "textellipsis",
    "LaTeX",
    "TeX",
    "LaTeXe",
    "textendash",
    "textemdash",
    "textquoteleft",
    "textquoteright",
    "textquotedblleft",
    "textquotedblright",
    "textregistered",
    "copyright",
    "texttrademark",
    "textbackslash",
    "textbar",
    "textless",
    "textgreater",
    "textasciitilde",
    "textasciicircum",
    "textbullet",
    "textdagger",
    "textdaggerdbl",
    "textsection",
    "textparagraph",
    "textdegree",
    "textperiodcentered",
    "textvisiblespace",
    "textunderscore",
    "textbraceleft",
    "textbraceright",
    "textdollar",
    "textsterling",
    "texteuro",
    "textminus",
    "textasteriskcentered",
    "slash",
    "textcompwordmark",
    "nobreakspace",
    "ensuremath",
    "lowercase",
    "uppercase",
    "MakeUppercase",
    "MakeLowercase",
    "today",
    // cross-references, notes, index (replayed from the last pass's aux)
    "ref",
    "pageref",
    "label",
    // manual bibliographies (thebibliography is a block environment)
    "bibitem",
    "newblock",
    "footnote",
    "footnotemark",
    "footnotetext",
    "index",
    "nocite",
    // lengths usable inside arguments
    "textwidth",
    "linewidth",
    "columnwidth",
    "textheight",
    "baselineskip",
    "parindent",
    "paperwidth",
    "paperheight",
    // alignment declarations (local to the environment they appear in)
    "centering",
    "raggedright",
    "raggedleft",
];

/// Declarations that are only allowed *inside* a brace group or an environment (they would
/// otherwise leak).
const GROUP_ONLY_DECLARATIONS: &[&str] = &[
    "itshape",
    "bfseries",
    "scshape",
    "sffamily",
    "ttfamily",
    "rmfamily",
    "mdseries",
    "upshape",
    "slshape",
    "normalfont",
    "em",
    "tiny",
    "scriptsize",
    "footnotesize",
    "small",
    "normalsize",
    "large",
    "Large",
    "LARGE",
    "huge",
    "Huge",
];

const MATH_MACROS: &[&str] = &[
    "frac",
    "dfrac",
    "tfrac",
    "sqrt",
    "sum",
    "prod",
    "int",
    "iint",
    "oint",
    "lim",
    "limsup",
    "liminf",
    "infty",
    "cdot",
    "cdots",
    "ldots",
    "dots",
    "vdots",
    "ddots",
    "times",
    "div",
    "pm",
    "mp",
    "leq",
    "le",
    "geq",
    "ge",
    "neq",
    "ne",
    "approx",
    "equiv",
    "sim",
    "simeq",
    "cong",
    "propto",
    "to",
    "rightarrow",
    "leftarrow",
    "Rightarrow",
    "Leftarrow",
    "leftrightarrow",
    "Leftrightarrow",
    "mapsto",
    "longrightarrow",
    "in",
    "notin",
    "ni",
    "subset",
    "subseteq",
    "supset",
    "supseteq",
    "cup",
    "cap",
    "setminus",
    "emptyset",
    "varnothing",
    "forall",
    "exists",
    "nexists",
    "neg",
    "lnot",
    "land",
    "lor",
    "wedge",
    "vee",
    "partial",
    "nabla",
    "prime",
    "angle",
    "perp",
    "parallel",
    "mid",
    "circ",
    "bullet",
    "star",
    "ast",
    "oplus",
    "otimes",
    "odot",
    "mathrm",
    "mathbf",
    "mathit",
    "mathsf",
    "mathtt",
    "mathcal",
    "mathbb",
    "mathfrak",
    "mathscr",
    "boldsymbol",
    "operatorname",
    "text",
    "textrm",
    "textit",
    "textbf",
    "mbox",
    "left",
    "right",
    "bigl",
    "bigr",
    "Bigl",
    "Bigr",
    "biggl",
    "biggr",
    "Biggl",
    "Biggr",
    "big",
    "Big",
    "bigg",
    "Bigg",
    "langle",
    "rangle",
    "lfloor",
    "rfloor",
    "lceil",
    "rceil",
    "vert",
    "Vert",
    "|",
    "{",
    "}",
    "lbrace",
    "rbrace",
    "backslash",
    "hat",
    "bar",
    "vec",
    "tilde",
    "dot",
    "ddot",
    "overline",
    "underline",
    "widehat",
    "widetilde",
    "overrightarrow",
    "overleftarrow",
    "acute",
    "grave",
    "breve",
    "check",
    "binom",
    "dbinom",
    "tbinom",
    "choose",
    "over",
    "atop",
    "stackrel",
    "overset",
    "underset",
    "substack",
    "lvert",
    "rvert",
    "lVert",
    "rVert",
    "overbrace",
    "underbrace",
    "xrightarrow",
    "xleftarrow",
    "iiint",
    "bigcup",
    "bigcap",
    "bigoplus",
    "coprod",
    "implies",
    "impliedby",
    "iff",
    "pmod",
    "bmod",
    "mod",
    "dotsc",
    "dotsb",
    "cfrac",
    "subsetneq",
    "supsetneq",
    "sqsubseteq",
    "triangleq",
    "coloneqq",
    "checkmark",
    "operatorname*",
    "sin",
    "cos",
    "tan",
    "cot",
    "sec",
    "csc",
    "arcsin",
    "arccos",
    "arctan",
    "sinh",
    "cosh",
    "tanh",
    "log",
    "ln",
    "lg",
    "exp",
    "min",
    "max",
    "sup",
    "inf",
    "arg",
    "det",
    "dim",
    "deg",
    "gcd",
    "hom",
    "ker",
    "Pr",
    "alpha",
    "beta",
    "gamma",
    "delta",
    "epsilon",
    "varepsilon",
    "zeta",
    "eta",
    "theta",
    "vartheta",
    "iota",
    "kappa",
    "lambda",
    "mu",
    "nu",
    "xi",
    "pi",
    "varpi",
    "rho",
    "varrho",
    "sigma",
    "varsigma",
    "tau",
    "upsilon",
    "phi",
    "varphi",
    "chi",
    "psi",
    "omega",
    "Gamma",
    "Delta",
    "Theta",
    "Lambda",
    "Xi",
    "Pi",
    "Sigma",
    "Upsilon",
    "Phi",
    "Psi",
    "Omega",
    "ell",
    "hbar",
    "imath",
    "jmath",
    "aleph",
    "Re",
    "Im",
    "wp",
    "top",
    "bot",
    "dagger",
    "ddagger",
    "colon",
    "quad",
    "qquad",
    ",",
    ";",
    ":",
    "!",
    " ",
    "displaystyle",
    "textstyle",
    "scriptstyle",
    "scriptscriptstyle",
    "limits",
    "nolimits",
    "not",
    "phantom",
    "hphantom",
    "vphantom",
    "label",
    "%",
    "$",
    "#",
    "&",
    "_",
    "\\",
    "nonumber",
    "notag",
    "tag",
    "tag*",
    "intertext",
    "eqref",
    "ref",
    "qquad",
    "hspace",
    "hfill",
    "quad",
    "left.",
    "right.",
    "mathop",
    "mathbin",
    "mathrel",
    "mathord",
    "mathpunct",
    "mathinner",
    "mathopen",
    "mathclose",
    "allowbreak",
    "textcolor",
    "color",
];

/// Patterns that make a unit background-only regardless of allow-lists.
const HARD_STOPS: &[(&str, Reason)] = &[];

/// Macros provided by a package: allowed when the package is loaded, `NeedsPackage` otherwise.
const PKG_MACROS: &[(&str, &[&str])] = &[
    (
        "hyperref",
        &[
            "href",
            "url",
            "autoref",
            "nameref",
            "hyperref",
            "phantomsection",
            "texorpdfstring",
        ],
    ),
    ("caption", &["captionof", "captionof*"]),
    (
        "biblatex",
        &[
            "parencite",
            "textcite",
            "autocite",
            "footcite",
            "citeauthor",
            "citetitle",
            "citeyear",
            "citedate",
            "Parencite",
            "Textcite",
            "Autocite",
            "Citeauthor",
            "smartcite",
            "supercite",
            "fullcite",
            "footfullcite",
            "nocite",
        ],
    ),
    (
        "natbib",
        &[
            "citep",
            "citet",
            "citealp",
            "citealt",
            "citeauthor",
            "citeyear",
            "citeyearpar",
            "citenum",
            "Citep",
            "Citet",
            "citetext",
        ],
    ),
    (
        "siunitx",
        &[
            "SI", "si", "num", "SIrange", "numrange", "qty", "unit", "ang", "qtyrange", "numlist",
            "qtylist",
        ],
    ),
    (
        "ulem",
        &[
            "sout",
            "uline",
            "uwave",
            "xout",
            "dashuline",
            "dotuline",
            "uuline",
        ],
    ),
    ("soul", &["hl", "so", "ul", "st", "caps"]),
    ("listings", &["lstinline"]),
    ("multirow", &["multirow"]),
    (
        "colortbl",
        &["rowcolor", "cellcolor", "rowcolors", "arrayrulecolor"],
    ),
    ("cancel", &["cancel", "bcancel", "xcancel", "cancelto"]),
    ("bm", &["bm"]),
    (
        "amssymb",
        &[
            "mathbb",
            "checkmark",
            "square",
            "blacksquare",
            "lesssim",
            "gtrsim",
            "nexists",
            "varnothing",
            "therefore",
            "because",
        ],
    ),
];

/// Is `name` a package macro? `Some(Ok(()))` when one of the packages providing it is loaded,
/// `Some(Err(first provider))` when none is, `None` for macros no package list names.
fn package_macro(name: &str, policy: &Policy) -> Option<Result<(), &'static str>> {
    let mut first: Option<&'static str> = None;
    for (pkg, ms) in PKG_MACROS {
        if ms.contains(&name) {
            if policy.has_package(pkg) {
                return Some(Ok(()));
            }
            first.get_or_insert(pkg);
        }
    }
    first.map(Err)
}

/// Macros whose argument(s) are opaque (URLs, units, verbatim): skipped, not classified.
/// (name, brace arguments to skip; 0 = delimited like \verb|…|).
const OPAQUE_ARGS: &[(&str, usize)] = &[
    ("url", 1),
    ("href", 1),
    ("nolinkurl", 1),
    ("path", 1),
    ("SI", 2),
    ("si", 1),
    ("num", 1),
    ("SIrange", 3),
    ("numrange", 2),
    ("qty", 2),
    ("unit", 1),
    ("ang", 1),
    ("qtyrange", 3),
    ("numlist", 1),
    ("qtylist", 2),
    ("verb", 0),
    ("verb*", 0),
    ("lstinline", 0),
];

/// Environments whose body is opaque (verbatim material): skipped to `\end{name}`.
const OPAQUE_ENVS: &[&str] = &["verbatim", "verbatim*", "lstlisting", "Verbatim", "alltt"];

/// Settings that are fine inside a group or environment (local), not at a unit's top level,
/// where they would change the document state the next units are typeset in.
const GROUP_ONLY_SETTERS: &[&str] = &["setlength", "addtolength", "setstretch", "linespread"];

/// Block environments that form units (or end a paragraph unit) and may nest.
pub use crate::document::BLOCK_ENVS;
pub const FLOAT_ENVS: &[&str] = &["figure", "figure*", "table", "table*"];
/// Display-math environments (inside paragraph units).
pub const DISPLAY_ENVS: &[&str] = &[
    "equation",
    "equation*",
    "displaymath",
    "eqnarray",
    "eqnarray*",
];
pub const AMSMATH_DISPLAY_ENVS: &[&str] = &[
    "align",
    "align*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "flalign",
    "flalign*",
    "alignat",
    "alignat*",
];
/// Environments allowed inside units (not units themselves).
const INNER_ENVS: &[&str] = &[
    "tabular",
    "tabular*",
    "tabularx",
    "minipage",
    "subfigure",
    "subtable",
    "math",
    "em",
    "small",
    "footnotesize",
    "scriptsize",
    "large",
    "Large",
    "bfseries",
    "itshape",
    "sloppypar",
];
const AMSMATH_INNER_ENVS: &[&str] = &[
    "split",
    "aligned",
    "gathered",
    "cases",
    "pmatrix",
    "bmatrix",
    "vmatrix",
    "Vmatrix",
    "matrix",
    "smallmatrix",
    "array",
    "subarray",
];
const LIST_ENVS: &[&str] = &["itemize", "enumerate", "description"];
const TABULAR_MACROS: &[&str] = &["hline", "cline", "multicolumn", "multirow"];
const BOOKTABS_MACROS: &[&str] = &[
    "toprule",
    "midrule",
    "bottomrule",
    "cmidrule",
    "addlinespace",
    "specialrule",
];
const FLOAT_MACROS: &[&str] = &["caption", "caption*"];
const HEADINGS: &[&str] = &[
    "part",
    "chapter",
    "section",
    "subsection",
    "subsubsection",
    "paragraph",
    "subparagraph",
];

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Extra text-mode macros the host vouches for (pure, no global side effects).
    pub trusted_macros: BTreeSet<String>,
    /// Theorem-like environments (from `\newtheorem` in the preamble or the host) treated as units.
    pub theorem_envs: BTreeSet<String>,
    pub amsmath: bool,
    pub graphicx: bool,
    pub booktabs: bool,
    pub tabularx: bool,
    /// `\cite` is replayable from the aux (plain or natbib `\bibcite`); false with biblatex.
    pub cite_ok: bool,
    /// Packages loaded by the preamble (`\usepackage`/`\RequirePackage`, options resolved for
    /// `xcolor[table]` → colortbl).
    pub packages: BTreeSet<String>,
    /// Math-mode macros defined in the preamble whose bodies are allow-listed (`\newcommand{\R}{\mathbb{R}}`).
    pub trusted_math: BTreeSet<String>,
    /// Environments defined in the preamble (`\newenvironment`) whose begin/end code is
    /// allow-listed text (font switches, `\noindent`, a label): typeset inside the paragraph
    /// they wrap. Ones whose code opens a block environment are in `theorem_envs` instead.
    pub user_inner_envs: BTreeSet<String>,
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
                        if text[s + bo + 1..s + bo + bc]
                            .split(',')
                            .any(|n| n.trim() == name)
                        {
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
        let mut packages = BTreeSet::new();
        for (opts, names) in usepackages(&text) {
            for n in names {
                packages.insert(n.clone());
                if n == "xcolor" && opts.iter().any(|o| o == "table") {
                    packages.insert("colortbl".into());
                }
                if n == "hyperref" {
                    packages.insert("url".into());
                }
                if n == "mathtools" {
                    packages.insert("amsmath".into());
                }
                // \mathbb and the AMS symbols also come with amsfonts and unicode-math
                if n == "amsfonts" || n == "unicode-math" {
                    packages.insert("amssymb".into());
                }
            }
        }
        let mut policy = Policy {
            trusted_macros: trusted.iter().cloned().collect(),
            theorem_envs,
            amsmath: has_pkg("amsmath") || has_pkg("mathtools"),
            graphicx: has_pkg("graphicx") || has_pkg("graphics"),
            booktabs: has_pkg("booktabs"),
            tabularx: has_pkg("tabularx"),
            cite_ok: true,
            packages,
            trusted_math: BTreeSet::new(),
            user_inner_envs: BTreeSet::new(),
        };
        // Macros defined in the preamble whose bodies are themselves allow-listed are trusted:
        // in text mode, in math mode, or both, depending on how the body classifies. Two rounds
        // let a macro use one defined before it.
        let defs = user_macro_definitions(&text);
        for _round in 0..2 {
            for (name, nargs, body) in &defs {
                let mut b = body.clone();
                for k in 1..=*nargs {
                    b = b.replace(&format!("#{k}"), "x");
                }
                if b.contains('#')
                    || b.contains("\\def")
                    || b.contains("\\let")
                    || b.contains("\\global")
                    || b.contains("\\gdef")
                    || b.contains("\\xdef")
                    || b.contains("\\newcommand")
                    || b.contains("\\renewcommand")
                {
                    continue;
                }
                if classify_source(&format!("x {b} x"), &policy).1.is_empty() {
                    policy.trusted_macros.insert(name.clone());
                }
                if classify_source(&format!("$x {b} x$"), &policy).1.is_empty() {
                    policy.trusted_math.insert(name.clone());
                }
            }
        }
        // Environments defined in the preamble: allowed when their begin and end code is
        // allow-listed. One that opens a block environment (quote, itemize, center, a theorem)
        // is a block unit itself (the capture tags it like a theorem); otherwise it is typeset
        // inside the paragraph that uses it.
        for (name, nargs, begin, end) in user_environment_definitions(&text) {
            let mut b = format!("{begin} x {end}");
            for k in 1..=nargs {
                b = b.replace(&format!("#{k}"), "x");
            }
            if b.contains('#')
                || b.contains("\\def")
                || b.contains("\\let")
                || b.contains("\\global")
                || b.contains("\\gdef")
                || b.contains("\\xdef")
                || b.contains("\\newcommand")
                || b.contains("\\renewcommand")
                || name.is_empty()
            {
                continue;
            }
            let (shape, mut reasons) = classify_source(&b, &policy);
            // declarations in the begin code are local to the environment's group
            reasons.retain(|r| !matches!(r, Reason::SizeDeclarationOutsideGroup(_)));
            if !reasons.is_empty() {
                continue;
            }
            match shape {
                UnitShape::Env(_) => {
                    policy.theorem_envs.insert(name);
                }
                UnitShape::Par => {
                    policy.user_inner_envs.insert(name);
                }
                UnitShape::Heading(_) => {}
            }
        }
        policy
    }

    pub fn has_package(&self, name: &str) -> bool {
        self.packages.contains(name)
    }

    pub fn is_block_env(&self, name: &str) -> bool {
        BLOCK_ENVS.contains(&name) || FLOAT_ENVS.contains(&name) || self.theorem_envs.contains(name)
    }
    pub fn is_display_env(&self, name: &str) -> bool {
        DISPLAY_ENVS.contains(&name) || AMSMATH_DISPLAY_ENVS.contains(&name)
    }
    /// Comma-separated extra unit environments for the capture run (`$RTEX_UNIT_ENVS`).
    pub fn unit_envs_env(&self) -> String {
        self.theorem_envs
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// `\usepackage[opts]{a,b}` / `\RequirePackage` occurrences as (options, names).
fn usepackages(text: &str) -> Vec<(Vec<String>, Vec<String>)> {
    let mut out = Vec::new();
    for key in ["\\usepackage", "\\RequirePackage"] {
        let mut idx = 0;
        while let Some(p) = text[idx..].find(key) {
            let s = idx + p + key.len();
            let rest = text[s..].trim_start();
            let mut opts = Vec::new();
            let mut r = rest;
            if let Some(after) = r.strip_prefix('[') {
                if let Some(e) = after.find(']') {
                    opts = after[..e]
                        .split(',')
                        .map(|o| o.split('=').next().unwrap_or("").trim().to_string())
                        .collect();
                    r = after[e + 1..].trim_start();
                }
            }
            if let Some(after) = r.strip_prefix('{') {
                if let Some(e) = after.find('}') {
                    let names = after[..e]
                        .split(',')
                        .map(|n| n.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .collect();
                    out.push((opts, names));
                }
            }
            idx = s;
        }
    }
    out
}

/// Simple macro definitions in the preamble: `\newcommand{\name}[n]{body}` (also `*`,
/// `\renewcommand`, `\providecommand`, an optional default argument), `\DeclareMathOperator`
/// and parameterless `\def\name{body}`. Returns (name, argument count, body).
/// `\newenvironment{name}[n][default]{begin}{end}` / `\renewenvironment` in `text`:
/// (name, argument count, begin code, end code).
fn user_environment_definitions(text: &str) -> Vec<(String, usize, String, String)> {
    fn balanced(text: &str, open_at: usize) -> Option<usize> {
        let b = text.as_bytes();
        if b.get(open_at) != Some(&b'{') {
            return None;
        }
        let mut depth = 0i32;
        let mut i = open_at;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 1,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }
    let mut out = Vec::new();
    let b = text.as_bytes();
    for key in ["\\newenvironment", "\\renewenvironment"] {
        let mut idx = 0;
        while let Some(p) = text[idx..].find(key) {
            let mut i = idx + p + key.len();
            idx = i;
            if i < b.len() && b[i] == b'*' {
                i += 1;
            }
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            let Some(e) = balanced(text, i) else { continue };
            let name = text[i + 1..e].trim().to_string();
            i = e + 1;
            let mut nargs = 0usize;
            let mut has_default = false;
            for round in 0..2 {
                while i < b.len() && (b[i] == b' ' || b[i] == b'\n') {
                    i += 1;
                }
                if i < b.len() && b[i] == b'[' {
                    if let Some(k) = text[i..].find(']') {
                        if round == 0 {
                            nargs = text[i + 1..i + k].trim().parse().unwrap_or(0);
                        } else {
                            has_default = true;
                        }
                        i += k + 1;
                    }
                }
            }
            while i < b.len() && (b[i] == b' ' || b[i] == b'\n') {
                i += 1;
            }
            let Some(e1) = balanced(text, i) else {
                continue;
            };
            let begin = text[i + 1..e1].to_string();
            i = e1 + 1;
            while i < b.len() && (b[i] == b' ' || b[i] == b'\n') {
                i += 1;
            }
            let Some(e2) = balanced(text, i) else {
                continue;
            };
            let end = text[i + 1..e2].to_string();
            out.push((name, nargs.max(if has_default { 1 } else { 0 }), begin, end));
        }
    }
    out
}

fn user_macro_definitions(text: &str) -> Vec<(String, usize, String)> {
    fn balanced(text: &str, open_at: usize) -> Option<usize> {
        let b = text.as_bytes();
        if b.get(open_at) != Some(&b'{') {
            return None;
        }
        let mut depth = 0i32;
        let mut i = open_at;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 1,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }
    let mut out = Vec::new();
    let b = text.as_bytes();
    for key in [
        "\\newcommand",
        "\\renewcommand",
        "\\providecommand",
        "\\DeclareMathOperator",
        "\\def",
    ] {
        let mut idx = 0;
        while let Some(p) = text[idx..].find(key) {
            let mut i = idx + p + key.len();
            idx = i;
            if i < b.len() && b[i] == b'*' {
                i += 1;
            }
            // control sequence, braced or bare
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            let braced = i < b.len() && b[i] == b'{';
            if braced {
                i += 1;
            }
            if i >= b.len() || b[i] != b'\\' {
                continue;
            }
            let mut j = i + 1;
            while j < b.len() && (b[j] as char).is_ascii_alphabetic() {
                j += 1;
            }
            if j == i + 1 {
                continue;
            }
            let name = text[i + 1..j].to_string();
            i = j;
            if braced {
                if i < b.len() && b[i] == b'}' {
                    i += 1;
                } else {
                    continue;
                }
            }
            if key == "\\DeclareMathOperator" {
                out.push((name, 0, "\\operatorname{x}".to_string()));
                continue;
            }
            if key == "\\def" {
                // only the parameterless form
                if i < b.len() && b[i] == b'{' {
                    if let Some(e) = balanced(text, i) {
                        out.push((name, 0, text[i + 1..e].to_string()));
                    }
                }
                continue;
            }
            let mut nargs = 0usize;
            let mut has_default = false;
            for _ in 0..2 {
                if i < b.len() && b[i] == b'[' {
                    if let Some(e) = text[i..].find(']') {
                        let inner = text[i + 1..i + e].trim();
                        if nargs == 0 && !has_default {
                            nargs = inner.parse().unwrap_or(0);
                        } else {
                            has_default = true;
                        }
                        if nargs == 0 && !has_default {
                            has_default = true;
                        }
                        i += e + 1;
                    }
                }
            }
            if i < b.len() && b[i] == b'{' {
                if let Some(e) = balanced(text, i) {
                    out.push((
                        name,
                        nargs.max(if has_default { 1 } else { 0 }),
                        text[i + 1..e].to_string(),
                    ));
                }
            }
        }
    }
    out
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
                // \verb<d>...<d> (and \lstinline): a % inside is literal
                for key in ["\\verb*", "\\verb", "\\lstinline"] {
                    if line[i..].starts_with(key)
                        && i + key.len() < bytes.len()
                        && !(bytes[i + key.len()] as char).is_ascii_alphabetic()
                    {
                        let d = bytes[i + key.len()];
                        let close = if d == b'{' { b'}' } else { d };
                        let start = i + key.len() + 1;
                        if let Some(e) = bytes[start..].iter().position(|&c| c == close) {
                            i = start + e + 1;
                        } else {
                            i = bytes.len();
                        }
                        break;
                    }
                }
                if i >= bytes.len() || bytes[i] != b'\\' {
                    continue;
                }
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
    // shape: a block/float/theorem environment spanning the whole unit (possibly after vertical
    // material such as \vspace or \noindent, which the unit box absorbs), a heading, or a paragraph
    let mut shape = UnitShape::Par;
    let env_start = {
        let mut k = 0;
        loop {
            let rest = trimmed[k..].trim_start();
            let off = trimmed.len() - rest.len();
            let mut matched = None;
            for v in [
                "\\vspace*",
                "\\vspace",
                "\\noindent",
                "\\newpage",
                "\\clearpage",
                "\\pagebreak",
                "\\smallskip",
                "\\medskip",
                "\\bigskip",
                "\\vfill",
                "\\centering",
            ] {
                if rest.starts_with(v)
                    && !rest[v.len()..].starts_with(|c: char| c.is_ascii_alphabetic())
                {
                    let mut e = v.len();
                    if rest[e..].starts_with('{') {
                        if let Some(c) = rest[e..].find('}') {
                            e += c + 1;
                        }
                    }
                    matched = Some(off + e);
                    break;
                }
            }
            match matched {
                Some(e) => k = e,
                None => break off,
            }
        }
    };
    if let Some((name, _)) = env_name(trimmed, env_start, "\\begin") {
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
    {
        let t = text.trim_start();
        for m in [
            "setcounter",
            "addtocounter",
            "stepcounter",
            "refstepcounter",
        ] {
            if t.starts_with(&format!("\\{m}{{")) && !t.trim_end().ends_with('}') {
                push(&mut reasons, Reason::LeadingCounter(m.into()));
                break;
            }
        }
    }
    let b = text.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut stack: Vec<Frame> = vec![Frame {
        mode: Mode::Text,
        env: None,
        depth_at_entry: 0,
    }];
    let mut env_depth = 0; // block environments open
    let mut block_env_closed_at: Option<usize> = None; // index after the last `\end{block}` at env_depth 0
    let mut text_group_pending = false; // next brace group is text mode (\text{...} in math)
    let mut line_start = 0usize;
    while i < b.len() {
        let c = b[i];
        // paragraph breaks (blank lines) are only fine inside block environments
        if c == b'\n' {
            let line = &text[line_start..i];
            if line.trim().is_empty()
                && i > 0
                && env_depth == 0
                && i + 1 < b.len()
                && !text[i + 1..].trim().is_empty()
                && line_start > 0
            {
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
                if j < b.len()
                    && b[j] == b'*'
                    && matches!(
                        name.as_str(),
                        "tag"
                            | "caption"
                            | "hspace"
                            | "vspace"
                            | "section"
                            | "subsection"
                            | "subsubsection"
                            | "chapter"
                            | "part"
                            | "paragraph"
                            | "subparagraph"
                            | "alph"
                            | "Alph"
                            | "arabic"
                            | "roman"
                            | "Roman"
                            | "operatorname"
                            | "verb"
                    )
                {
                    i = j + 1;
                } else {
                    i = j;
                }
            } else if n == b'(' {
                if in_math {
                    push(&mut reasons, Reason::UnbalancedMath);
                }
                stack.push(Frame {
                    mode: Mode::Math,
                    env: None,
                    depth_at_entry: depth,
                });
                i += 2;
                continue;
            } else if n == b')' {
                if !in_math
                    || stack
                        .last()
                        .map(|f| f.depth_at_entry != depth)
                        .unwrap_or(true)
                {
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
                stack.push(Frame {
                    mode: Mode::Math,
                    env: None,
                    depth_at_entry: depth,
                });
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
                if OPAQUE_ENVS.contains(&env.as_str()) {
                    // verbatim material: skip to \end{env}; the environment is a block unit
                    if env == "lstlisting" && !policy.has_package("listings") {
                        push(&mut reasons, Reason::NeedsPackage("listings".into()));
                    }
                    let end = format!("\\end{{{env}}}");
                    match text[i..].find(&end) {
                        Some(e) => {
                            i += e + end.len();
                            if policy.is_block_env(&env) && env_depth == 0 {
                                block_env_closed_at = Some(i);
                            }
                        }
                        None => push(&mut reasons, Reason::UnbalancedEnvironment),
                    }
                    continue;
                }
                if policy.is_display_env(&env) {
                    if is_amsmath_display && !policy.amsmath {
                        push(&mut reasons, Reason::NeedsPackage("amsmath".into()));
                    }
                    if in_math {
                        push(&mut reasons, Reason::UnbalancedMath);
                    }
                    stack.push(Frame {
                        mode: Mode::Math,
                        env: Some(env),
                        depth_at_entry: depth,
                    });
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
                    stack.push(Frame {
                        mode: Mode::Text,
                        env: Some(env),
                        depth_at_entry: depth,
                    });
                } else if INNER_ENVS.contains(&env.as_str())
                    || is_amsmath_inner
                    || policy.user_inner_envs.contains(&env)
                {
                    if is_amsmath_inner && !policy.amsmath {
                        push(&mut reasons, Reason::NeedsPackage("amsmath".into()));
                    }
                    if env == "tabularx" && !policy.tabularx {
                        push(&mut reasons, Reason::NeedsPackage("tabularx".into()));
                    }
                    let mode = if is_amsmath_inner || env == "math" {
                        Mode::Math
                    } else {
                        Mode::Text
                    };
                    if env == "math" && in_math {
                        push(&mut reasons, Reason::UnbalancedMath);
                    }
                    stack.push(Frame {
                        mode: if is_amsmath_inner { Mode::Math } else { mode },
                        env: Some(env),
                        depth_at_entry: depth,
                    });
                } else {
                    push(&mut reasons, Reason::DisallowedEnvironment(env.clone()));
                    stack.push(Frame {
                        mode: Mode::Text,
                        env: Some(env),
                        depth_at_entry: depth,
                    });
                }
                continue;
            }
            if name == "end" {
                let Some((env, after)) = env_name(&text, i - 4, "\\end") else {
                    push(&mut reasons, Reason::UnbalancedEnvironment);
                    continue;
                };
                i = after;
                match stack
                    .iter()
                    .rposition(|f| f.env.as_deref() == Some(env.as_str()))
                {
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
            // opaque arguments (URLs, units, verbatim): skip them, check the package
            if let Some((_, nargs)) = OPAQUE_ARGS.iter().find(|(n, _)| *n == name.as_str()) {
                if let Some(Err(pkg)) = package_macro(&name, policy) {
                    push(&mut reasons, Reason::NeedsPackage(pkg.into()));
                }
                if *nargs == 0 {
                    // \verb|...| / \lstinline|...| / \lstinline{...}
                    if i < b.len() {
                        let d = b[i];
                        let close = if d == b'{' { b'}' } else { d };
                        match text[i + 1..].find(close as char) {
                            Some(e) => i = i + 1 + e + 1,
                            None => push(&mut reasons, Reason::Verbatim),
                        }
                    }
                } else {
                    // optional argument, then n balanced brace groups
                    while i < b.len() && b[i] == b' ' {
                        i += 1;
                    }
                    if i < b.len() && b[i] == b'[' {
                        if let Some(e) = text[i..].find(']') {
                            i += e + 1;
                        }
                    }
                    for _ in 0..*nargs {
                        while i < b.len() && b[i] == b' ' {
                            i += 1;
                        }
                        if i < b.len() && b[i] == b'{' {
                            let mut d = 0i32;
                            let mut k = i;
                            while k < b.len() {
                                match b[k] {
                                    b'\\' => k += 1,
                                    b'{' => d += 1,
                                    b'}' => {
                                        d -= 1;
                                        if d == 0 {
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                                k += 1;
                            }
                            i = (k + 1).min(b.len());
                        } else {
                            break;
                        }
                    }
                }
                continue;
            }
            // macros
            let allowed = if let Some(provided) = package_macro(&name, policy) {
                if let Err(pkg) = provided {
                    push(&mut reasons, Reason::NeedsPackage(pkg.into()));
                }
                if in_math && matches!(name.as_str(), "text" | "mbox") {
                    text_group_pending = true;
                }
                true
            } else if GROUP_ONLY_SETTERS.contains(&name.as_str())
                || ((name == "renewcommand" || name == "renewcommand*")
                    && text[i..].trim_start().starts_with("{\\arraystretch}"))
            {
                if depth == 0 && env_depth == 0 {
                    push(
                        &mut reasons,
                        Reason::SizeDeclarationOutsideGroup(name.clone()),
                    );
                }
                // the first argument names the length or macro being set: skip it
                let rest = &text[i..];
                let lead = rest.len() - rest.trim_start().len();
                if let Some(after) = rest.trim_start().strip_prefix('{') {
                    if let Some(e) = after.find('}') {
                        i += lead + 1 + e + 1;
                    }
                }
                true
            } else if in_math {
                let ok = MATH_MACROS.contains(&name.as_str())
                    || policy.trusted_math.contains(&name)
                    || policy.trusted_macros.contains(&name);
                if ok
                    && matches!(
                        name.as_str(),
                        "text" | "mbox" | "textrm" | "textit" | "textbf" | "intertext"
                    )
                {
                    text_group_pending = true;
                }
                if (name == "tag"
                    || name == "notag"
                    || name == "nonumber"
                    || name == "intertext"
                    || name == "eqref")
                    && !policy.amsmath
                    && name != "nonumber"
                {
                    push(&mut reasons, Reason::NeedsPackage("amsmath".into()));
                }
                ok
            } else if GROUP_ONLY_DECLARATIONS.contains(&name.as_str()) {
                if depth == 0 && env_depth == 0 && !matches!(shape, UnitShape::Heading(_)) {
                    push(
                        &mut reasons,
                        Reason::SizeDeclarationOutsideGroup(name.clone()),
                    );
                }
                true
            } else if HEADINGS.contains(&name.as_str()) {
                // only as the unit's own heading command
                matches!(&shape, UnitShape::Heading(h) if *h == name)
                    && i <= name.len() + 2 + text.len() - trimmed.len()
            } else if name == "item" {
                let in_list = stack.iter().any(|f| {
                    f.env
                        .as_deref()
                        .map(|e| LIST_ENVS.contains(&e))
                        .unwrap_or(false)
                });
                if !in_list {
                    push(&mut reasons, Reason::MacroOutsideContext(name.clone()));
                }
                true
            } else if FLOAT_MACROS.contains(&name.as_str()) {
                let in_float = stack.iter().any(|f| {
                    f.env
                        .as_deref()
                        .map(|e| FLOAT_ENVS.contains(&e))
                        .unwrap_or(false)
                });
                if !in_float {
                    push(&mut reasons, Reason::MacroOutsideContext(name.clone()));
                }
                true
            } else if TABULAR_MACROS.contains(&name.as_str())
                || BOOKTABS_MACROS.contains(&name.as_str())
            {
                let in_tab = stack.iter().any(|f| {
                    f.env
                        .as_deref()
                        .map(|e| e.starts_with("tabular") || e == "array")
                        .unwrap_or(false)
                });
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
            } else if name == "cite" {
                if !policy.cite_ok {
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
                let r = if in_math {
                    Reason::DisallowedMathMacro(name)
                } else {
                    Reason::DisallowedMacro(name)
                };
                push(&mut reasons, r);
            }
            continue;
        }
        match c {
            b'{' => {
                depth += 1;
                if text_group_pending {
                    text_group_pending = false;
                    stack.push(Frame {
                        mode: Mode::Text,
                        env: None,
                        depth_at_entry: depth,
                    });
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
                    if f.env.is_none()
                        && f.mode == Mode::Text
                        && stack.len() > 1
                        && f.depth_at_entry == depth + 1
                    {
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
                    stack.push(Frame {
                        mode: Mode::Math,
                        env: None,
                        depth_at_entry: depth,
                    });
                }
            }
            _ => {
                if block_env_closed_at.is_some()
                    && env_depth == 0
                    && shape == UnitShape::Par
                    && !c.is_ascii_whitespace()
                {
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
    let e = ep
        .trim_start_matches("\\leftprotrusion ")
        .trim_end_matches("\\leftprotrusion ");
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
    PATTERNS.contains(&e)
}

/// Engine-side check of a unit from the capture: `everypar` pattern, node flags, members.
pub fn check_engine_unit(
    kind: &str,
    everypar: &str,
    has_context: bool,
    rows: i64,
    flags: &std::collections::BTreeMap<String, i64>,
) -> Vec<Reason> {
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
pub fn check_engine(
    groupcode: &str,
    nest: i64,
    everypar: &str,
    has_begin: bool,
    flags: &std::collections::BTreeMap<String, i64>,
) -> Vec<Reason> {
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
        Policy {
            amsmath: true,
            graphicx: true,
            booktabs: true,
            tabularx: true,
            cite_ok: true,
            packages: [
                "hyperref", "url", "natbib", "siunitx", "ulem", "listings", "multirow", "colortbl",
                "cancel", "bm", "amssymb",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ..Default::default()
        }
    }
    fn ok(s: &str) -> bool {
        check_source(s, &pol()).is_empty()
    }
    fn reasons(s: &str) -> Vec<Reason> {
        check_source(s, &pol())
    }
    #[test]
    fn plain_and_fonts_ok() {
        assert!(ok(
            "Plain text, with \\emph{emphasis} and \\textbf{bold}, ``quotes'' and a dash---here."
        ));
        assert!(ok(
            "Inline $x^2 + \\frac{1}{2} \\leq \\alpha$ math and \\(a_i\\) too.\nSecond line."
        ));
        assert!(ok("{\\itshape grouped declaration} and {\\small small}."));
        assert!(ok(
            "A \\textcolor{red}{red} word and \\'e accents, 50\\% and \\&."
        ));
    }
    #[test]
    fn disallowed() {
        assert!(reasons("Footnote\\footnote{x} here.").is_empty());
        assert!(reasons("\\mymacro{x}").contains(&Reason::DisallowedMacro("mymacro".into())));
        assert!(reasons("Text \\large leaking.")
            .contains(&Reason::SizeDeclarationOutsideGroup("large".into())));
        assert!(reasons("one\n\ntwo").contains(&Reason::ParagraphBreak));
        // an explicit \par is fine: a span may hold several consecutive paragraphs (one unit)
        assert!(!reasons("a \\par b").contains(&Reason::ParagraphBreak));
        let r = reasons("{\\Large\\bfseries Title\\par}\nName \\hfill 22 September 2026");
        assert!(r.is_empty(), "{r:?}");
        assert!(!ok("a \\parbox-like? no: \\parskip is disallowed"));
        assert!(reasons("\\begin{tikzpicture}\\end{tikzpicture}")
            .contains(&Reason::DisallowedEnvironment("tikzpicture".into())));
        // opaque arguments and verbatim
        assert!(reasons(
            "see \\url{https://a.b/c_d%e#f} and \\href{https://x.y/_z}{the \\textbf{notes}}"
        )
        .is_empty());
        assert!(reasons("call \\verb|\\foo{bar}| and \\lstinline|add(2, 3)| here").is_empty());
        assert!(reasons("and \\verb+%percent+ signs").is_empty());
        assert_eq!(
            classify_source("\\vspace{1cm}\n\\begin{quote}x\\end{quote}", &pol()).0,
            UnitShape::Env("quote".into())
        );
        assert!(reasons("\\SI{2.5}{\\metre} and \\num{3.2e-1} \\si{\\ohm}").is_empty());
        assert!(reasons("\\begin{verbatim}\n\\foo \\bar{\n%x\n\\end{verbatim}").is_empty());
        assert!(
            reasons("\\begin{lstlisting}[language=C]\nint x; /* } */\n\\end{lstlisting}")
                .is_empty()
        );
        // package-gated macros without the package
        let bare = Policy::default();
        assert!(classify_source("\\url{x}", &bare)
            .1
            .contains(&Reason::NeedsPackage("hyperref".into())));
        assert!(classify_source("\\sout{x}", &bare)
            .1
            .contains(&Reason::NeedsPackage("ulem".into())));
        // natbib citations, boxes, counters, setters
        assert!(
            reasons("as \\citep{a} and \\citet[p.~3]{b} say \\citeauthor{a}").is_empty(),
            "{:?}",
            reasons("as \\citep{a} and \\citet[p.~3]{b} say \\citeauthor{a}")
        );
        assert!(reasons(
            "\\fbox{\\parbox{3cm}{x}} \\colorbox{yellow}{y} \\underline{z} \\rule{1cm}{1pt}"
        )
        .is_empty());
        assert!(reasons("\\begin{enumerate}[label=(\\alph*)]\\item a\\end{enumerate}").is_empty());
        let r = reasons("\\begin{table}\\setlength{\\tabcolsep}{2pt}\\renewcommand{\\arraystretch}{1.2}\\begin{tabular}{l}a\\end{tabular}\\end{table}");
        assert!(r.is_empty(), "{r:?}");
        assert!(reasons("\\setlength{\\parindent}{0pt} text")
            .contains(&Reason::SizeDeclarationOutsideGroup("setlength".into())));
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
        assert!(ok(
            "See Section~\\ref{sec:x} on page~\\pageref{sec:x} and \\cite{knuth}."
        ));
        let mut p = pol();
        p.amsmath = false;
        assert!(check_source("\\begin{align} a &= b \\end{align}", &p)
            .contains(&Reason::NeedsPackage("amsmath".into())));
        p.cite_ok = false;
        assert!(
            check_source("See \\cite{k}.", &p).contains(&Reason::DisallowedMacro("cite".into()))
        );
    }
    #[test]
    fn environments() {
        let (shape, r) = classify_source(
            "\\begin{itemize}\n\\item First\n\\item Second with $x$\n\\end{itemize}",
            &pol(),
        );
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
        assert!(
            reasons("Intro:\n\\begin{itemize}\\item a\\end{itemize}\nmore text")
                .contains(&Reason::TextAfterEnvironment)
        );
        // blank lines inside a block environment are fine
        assert!(ok("\\begin{quote}\nfirst\n\nsecond\n\\end{quote}"));
        let mut p = pol();
        p.theorem_envs.insert("theorem".into());
        assert_eq!(
            classify_source("\\begin{theorem}\\label{t}Let $x$.\\end{theorem}", &p).0,
            UnitShape::Env("theorem".into())
        );
    }
    #[test]
    fn headings() {
        let (shape, r) = classify_source("\\section{A \\emph{title}}\\label{sec:a}", &pol());
        assert_eq!(shape, UnitShape::Heading("section".into()));
        assert!(r.is_empty(), "{r:?}");
        assert!(reasons("Text \\section{x}").contains(&Reason::DisallowedMacro("section".into())));
    }
    #[test]
    fn counters_and_package_macros() {
        let p = pol();
        assert!(check_source(
            "Text \\stepcounter{foo} and \\setcounter{enumi}{3} \\addtocounter{x}{-1}.",
            &p
        )
        .is_empty());
        // before the first text the command runs before the unit's counters are captured
        assert!(check_source("\\stepcounter{equation}\nText with $x$.", &p)
            .contains(&Reason::LeadingCounter("stepcounter".into())));
        // a lone counter line is a unit of its own (no rows): fine
        assert!(check_source("\\setcounter{section}{3}", &p).is_empty());
        // pol() has hyperref loaded but not caption
        assert!(reasons("\\captionof{figure}{A caption}")
            .contains(&Reason::NeedsPackage("caption".into())));
        assert!(classify_source("\\section{\\texorpdfstring{$x$}{x}}", &p)
            .1
            .is_empty());
        let p0 = Policy::from_preamble("\\documentclass{article}\n", &[], &[]);
        assert!(check_source("\\section{\\texorpdfstring{$x$}{x}}", &p0)
            .contains(&Reason::NeedsPackage("hyperref".into())));
        let p2 = Policy::from_preamble("\\usepackage{caption}\\usepackage{hyperref}\n", &[], &[]);
        assert!(check_source("\\captionof{figure}{A caption}", &p2).is_empty());
        assert!(classify_source("\\section{\\texorpdfstring{$x$}{x}}", &p2)
            .1
            .is_empty());
    }
    #[test]
    fn user_environments() {
        let p = Policy::from_preamble(
            "\\newenvironment{solution}{\\par\\noindent\\textbf{Solution.}\\ \\itshape}{\\par}\n\\newenvironment{hint}[1]{\\begin{quote}\\small\\textbf{#1:}\\ }{\\end{quote}}\n\\newenvironment{bad}{\\tikz{x}}{}\n\\renewenvironment{abstract}{\\small}{\\par}\n",
            &[],
            &[],
        );
        assert!(
            p.user_inner_envs.contains("solution") && p.user_inner_envs.contains("abstract"),
            "inner {:?} block {:?}",
            p.user_inner_envs,
            p.theorem_envs
        );
        assert!(p.theorem_envs.contains("hint"));
        assert!(!p.user_inner_envs.contains("bad") && !p.theorem_envs.contains("bad"));
        let (shape, r) = classify_source("\\begin{solution}\nText.\n\\end{solution}", &p);
        assert!(r.is_empty(), "{r:?}");
        assert_eq!(shape, UnitShape::Par);
        let (shape, r) = classify_source("\\begin{hint}{Idea}\nText.\n\\end{hint}", &p);
        assert!(r.is_empty(), "{r:?}");
        assert_eq!(shape, UnitShape::Env("hint".into()));
        assert!(!check_source("\\begin{bad}x\\end{bad}", &p).is_empty());
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
        assert!(p.amsmath && p.graphicx && !p.booktabs && p.cite_ok);
        // user macros: trusted when their bodies are allow-listed, in the mode they classify in
        let p2 = Policy::from_preamble("\\usepackage{amsmath,amssymb}\n\\usepackage[table]{xcolor}\n\\newcommand{\\R}{\\mathbb{R}}\n\\newcommand{\\abs}[1]{\\left\\lvert #1 \\right\\rvert}\n\\newcommand{\\code}[1]{\\texttt{#1}}\n\\newcommand{\\bad}{\\tikz{x}}\n\\DeclareMathOperator*{\\argmax}{arg\\,max}\n\\newcommand{\\absR}[1]{\\abs{#1}\\in\\R}\n", &[], &[]);
        assert!(
            p2.trusted_math.contains("R")
                && p2.trusted_math.contains("abs")
                && p2.trusted_math.contains("argmax")
                && p2.trusted_math.contains("absR"),
            "math {:?} text {:?}",
            p2.trusted_math,
            p2.trusted_macros
        );
        assert!(
            p2.trusted_macros.contains("code")
                && !p2.trusted_macros.contains("bad")
                && !p2.trusted_math.contains("bad")
        );
        assert!(p2.has_package("colortbl"));
        assert!(classify_source("$\\abs{x} \\in \\R$ and \\code{y}", &p2)
            .1
            .is_empty());
        assert!(p.theorem_envs.contains("theorem"));
    }
    #[test]
    fn everypar_patterns() {
        assert!(everypar_allowed(""));
        assert!(everypar_allowed("\\leftprotrusion "));
        assert!(everypar_allowed(
            "{\\setbox \\z@ \\lastbox }\\everypar {}\\@endpefalse "
        ));
        assert!(!everypar_allowed("\\@itemlabel"));
        let mut flags = std::collections::BTreeMap::new();
        flags.insert("ins".to_string(), 1);
        assert!(check_engine_unit("par", "", true, 3, &flags).is_empty());
        flags.insert("dir".to_string(), 1);
        assert_eq!(
            check_engine_unit("par", "", true, 3, &flags),
            vec![Reason::EngineFlag("dir=1".into())]
        );
    }
}
