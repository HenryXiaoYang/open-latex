//! Bibliographies, indices and glossaries across passes and the live engine. Skipped without
//! lualatex.

use rtex_core::{Convergence, Edit, Event, Session, SessionConfig};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn session(name: &str, files: &[(&str, &str)]) -> Option<(Session, PathBuf, PathBuf)> {
    if rtex_core::texlive::TexLive::discover().is_err() {
        eprintln!("SKIP: no lualatex");
        return None;
    }
    let root = std::env::temp_dir().join(format!("rtex-bib-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    for (rel, text) in files {
        std::fs::write(project.join(rel), text).unwrap();
    }
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debounce = Duration::from_millis(50);
    cfg.fast_budget = Duration::from_millis(5000); // timing is not what these tests measure
    let build = cfg.build_dir.clone();
    Some((Session::open(cfg).unwrap(), project, build))
}

/// Wait for a layout that ends a run (provisional per-pass layouts skipped).
fn wait_converged(s: &Session) -> Event {
    let (ev, others) = s.wait_for(Duration::from_secs(180), |e| {
        matches!(
            e,
            Event::LayoutUpdate {
                convergence: Convergence::Converged | Convergence::PassLimitReached { .. },
                ..
            }
        )
    });
    for o in others {
        s.requeue(o);
    }
    ev.expect("no converged layout")
}

fn insert_after(s: &Session, project: &Path, needle: &str, text: &str) {
    let src = std::fs::read_to_string(project.join("main.tex")).unwrap();
    let at = src.find(needle).unwrap() + needle.len();
    s.apply_edit(
        "main.tex",
        Edit {
            start_byte: at,
            end_byte: at,
            text: text.into(),
        },
    )
    .unwrap();
}

const BIB: &str = "@book{knuth1984,author={Donald E. Knuth},title={The {\\TeX}book},\
                   publisher={Addison-Wesley},year={1984}}\n";

#[test]
fn biblatex_citations_are_live_in_a_session_opened_without_a_build() {
    // the server starts before any pass, so it has no .bbl: biblatex prints the bare key and
    // the probe would keep the citing paragraph on the pass for the whole session
    let main = "\\documentclass{article}\n\\usepackage[backend=bibtex]{biblatex}\n\
                \\addbibresource{refs.bib}\n\\begin{document}\n\
                First paragraph cites the book~\\cite{knuth1984} in running text.\n\n\
                A second paragraph without citations.\n\\printbibliography\n\\end{document}\n";
    let Some((s, project, _)) = session("biblatex", &[("main.tex", main), ("refs.bib", BIB)])
    else {
        return;
    };
    wait_converged(&s);
    // the server is replaced by one that read the layout's .bbl
    let (swap, others) = s.wait_for(Duration::from_secs(60), |e| {
        matches!(e, Event::EngineState { state, reason: Some(r), .. }
            if state == "Ready" && r.contains("bibliography"))
    });
    for o in others {
        s.requeue(o);
    }
    assert!(swap.is_some(), "no server with the new bibliography");
    insert_after(&s, &project, "in running text", " More words");
    let (ev, _) = s.wait_for(Duration::from_secs(30), |e| {
        matches!(
            e,
            Event::ParagraphUpdate { .. } | Event::BackgroundScheduled { .. }
        )
    });
    match ev {
        Some(Event::ParagraphUpdate { status, .. }) => assert_eq!(status, "ok"),
        other => panic!("the citing paragraph is not live: {other:?}"),
    }
}

#[test]
fn makeindex_runs_between_passes() {
    // makeindex with no file reads stdin (empty here) and writes stdout
    let have_makeindex = std::process::Command::new("makeindex")
        .arg("-q")
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok();
    if !have_makeindex {
        eprintln!("SKIP: no makeindex");
        return;
    }
    let main = "\\documentclass{article}\n\\usepackage{makeidx}\n\\makeindex\n\
                \\begin{document}\nAardvarks\\index{aardvark} and zebras\\index{zebra}.\n\
                \\printindex\n\\end{document}\n";
    let Some((s, _project, build)) = session("makeindex", &[("main.tex", main)]) else {
        return;
    };
    match wait_converged(&s) {
        Event::LayoutUpdate { convergence, .. } => {
            assert!(
                matches!(convergence, Convergence::Converged),
                "{convergence:?}"
            )
        }
        _ => unreachable!(),
    }
    let ind: Vec<String> = ["pass-0", "pass-1"]
        .iter()
        .filter_map(|d| std::fs::read_to_string(build.join("bg").join(d).join("main.ind")).ok())
        .collect();
    assert!(
        ind.iter()
            .any(|t| t.contains("aardvark") && t.contains("zebra")),
        "no index written: {ind:?}"
    );
}

#[test]
fn glossaries_first_use_does_not_leak_between_live_compiles() {
    // \gls sets the entry's first-use switch globally: compiled live, the first compile would
    // leave it set in the server and every later one would print the short form where the
    // document (first use) prints the long one
    let have_glossaries = std::process::Command::new("kpsewhich")
        .arg("glossaries.sty")
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if !have_glossaries {
        eprintln!("SKIP: no glossaries.sty");
        return;
    }
    let main = "\\documentclass{article}\n\\usepackage[acronym]{glossaries}\n\
                \\makenoidxglossaries\n\\newacronym{cpu}{CPU}{central processing unit}\n\
                \\begin{document}\nA paragraph that mentions the \\gls{cpu} for the first time.\n\n\
                A second paragraph.\n\\end{document}\n";
    let Some((s, project, _)) = session("glossaries", &[("main.tex", main)]) else {
        return;
    };
    wait_converged(&s);
    let mut seen = 0;
    for word in [" x", " y", " z"] {
        insert_after(&s, &project, "for the first time", word);
        // the edits go to the session's copy; the next insertion point is unchanged on disk,
        // so each word lands before the previous one
        let (ev, _) = s.wait_for(Duration::from_secs(30), |e| {
            matches!(
                e,
                Event::ParagraphUpdate { .. } | Event::BackgroundScheduled { .. }
            )
        });
        match ev {
            Some(Event::ParagraphUpdate { status, dl, .. }) => {
                assert_eq!(status, "ok");
                let text: String = dl
                    .lines
                    .iter()
                    .flat_map(|l| l.items.iter())
                    .filter_map(|i| match i {
                        rtex_dl::Item::Glyph { char, .. } => char::from_u32(*char as u32),
                        _ => None,
                    })
                    .collect();
                assert!(
                    text.contains("centralprocessingunit"),
                    "short form live: {text}"
                );
                seen += 1;
            }
            other => panic!("the unit is not live: {other:?}"),
        }
    }
    assert_eq!(seen, 3);
}

#[test]
fn imakeidx_options_reach_the_index_program() {
    let have_imakeidx = std::process::Command::new("kpsewhich")
        .arg("imakeidx.sty")
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if !have_imakeidx {
        eprintln!("SKIP: no imakeidx.sty");
        return;
    }
    // the style puts a marker before each letter group: present only if `-s marker.ist` was used
    let ist = "headings_flag 1\nheading_prefix \"\\\\item RTEXMARK \"\nheading_suffix \"\"\n";
    let main = "\\documentclass{article}\n\\usepackage{imakeidx}\n\
                \\makeindex[options=-s marker.ist]\n\\begin{document}\n\
                Aardvarks\\index{aardvark} and zebras\\index{zebra}.\n\
                \\printindex\n\\end{document}\n";
    let Some((s, _project, build)) =
        session("imakeidx", &[("main.tex", main), ("marker.ist", ist)])
    else {
        return;
    };
    wait_converged(&s);
    let ind: Vec<String> = ["pass-0", "pass-1"]
        .iter()
        .filter_map(|d| std::fs::read_to_string(build.join("bg").join(d).join("main.ind")).ok())
        .collect();
    assert!(
        ind.iter().any(|t| t.contains("RTEXMARK")),
        "the document's index style was not used: {ind:?}"
    );
}
