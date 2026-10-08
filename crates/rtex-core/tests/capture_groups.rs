//! Capture facts for paragraphs that start inside a group and for tabularx: every unit must
//! have rows, the base font must be the one outside the group, and tabularx must be eligible.
//! Skipped without lualatex.

use rtex_core::capture::run_capture_with;
use rtex_core::eligibility::{classify_source, Policy, UnitShape};
use rtex_core::texlive::TexLive;

const DOC: &str = r#"\documentclass[11pt]{article}
\usepackage[T1]{fontenc}
\usepackage{lmodern}
\usepackage{tabularx}
\begin{document}

A plain paragraph of ordinary text that is long enough to wrap onto a second line when typeset in the article class.

{\em An emphasised start} to a paragraph whose first horizontal material is inside a brace group, which then continues as ordinary text for a while.

{\bfseries Bold start.} Another paragraph starting with a group, continuing with enough words to make it wrap to a second line of text here.

\begin{tabularx}{\textwidth}{lX}
Term & Meaning of the term, explained at some length so that the X column wraps. \\
\end{tabularx}

Some text before the table.
\begin{tabularx}{0.8\textwidth}{lX}
alpha & beta gamma delta \\
\end{tabularx}
And text after it in the same paragraph.

A last paragraph of ordinary text.
\end{document}
"#;

#[test]
fn group_started_paragraphs_and_tabularx_have_rows_and_outer_fonts() {
    let Ok(tl) = TexLive::discover() else {
        eprintln!("SKIP: no lualatex");
        return;
    };
    let root = std::env::temp_dir().join(format!("rtex-groups-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("main.tex"), DOC).unwrap();
    let cap = run_capture_with(&tl, &project, "main.tex", &root.join("out"), true, "").unwrap();
    assert_eq!(
        cap.json.units.len(),
        6,
        "{:?}",
        cap.json
            .units
            .iter()
            .map(|u| (u.begin_line, u.end_line))
            .collect::<Vec<_>>()
    );
    for u in &cap.json.units {
        assert!(
            !u.placements.is_empty(),
            "unit at line {} has no rows",
            u.begin_line
        );
        assert_eq!(u.kind, "par");
    }
    // the base font of the group-started paragraphs is the upright body font, not \em / \bfseries
    let paras: std::collections::HashMap<i64, _> =
        cap.json.paragraphs.iter().map(|p| (p.seq, p)).collect();
    for u in &cap.json.units[1..3] {
        let first = u
            .seqs
            .iter()
            .filter_map(|s| paras.get(s))
            .find(|p| p.nest == 1 && p.begin.is_some())
            .expect("body paragraph");
        let nfss = &first.begin.as_ref().unwrap().nfss;
        assert_eq!(
            (nfss.series.as_deref(), nfss.shape.as_deref()),
            (Some("m"), Some("n")),
            "unit at line {}",
            u.begin_line
        );
    }
    // tabularx is eligible when the package is loaded, and only then
    let (pre, _) = rtex_core::split_preamble(DOC).unwrap();
    let pol = Policy::from_preamble(pre, &[], &[]);
    let src = "\\begin{tabularx}{\\textwidth}{lX}\nTerm & Meaning \\\\\n\\end{tabularx}\n";
    let (shape, reasons) = classify_source(src, &pol);
    assert_eq!(shape, UnitShape::Par);
    assert!(reasons.is_empty(), "{reasons:?}");
    let pol2 = Policy::from_preamble("\\documentclass{article}\n", &[], &[]);
    assert!(!classify_source(src, &pol2).1.is_empty());
}
