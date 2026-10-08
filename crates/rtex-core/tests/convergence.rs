//! Convergence contract: converged layouts equal a clean instrumented run of the final source,
//! the capture package leaves the PDF unchanged, bibliographies converge, degraded pages carry a
//! PDF fallback. Skipped without lualatex.

use rtex_core::background::{run_pass, BibTool};
use rtex_core::fixtures::{generate, FontSet, Variant};
use rtex_core::session::CompileStatus;
use rtex_core::texlive::TexLive;
use rtex_core::{Convergence, Edit, Event, Session, SessionConfig};
use std::time::Duration;

fn setup(
    name: &str,
    pages: u32,
    variant: Variant,
) -> Option<(TexLive, std::path::PathBuf, std::path::PathBuf)> {
    let tl = match TexLive::discover() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SKIP: {e}");
            return None;
        }
    };
    let root = std::env::temp_dir().join(format!("rtex-conv-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    generate(pages, variant, FontSet::Pagella, 5, &project).unwrap();
    Some((tl, root, project))
}

fn wait_layout(s: &Session, secs: u64) -> Event {
    let (ev, _) = s.wait_for(Duration::from_secs(secs), |e| {
        matches!(e, Event::LayoutUpdate { .. })
    });
    ev.expect("layout update")
}

#[test]
fn converged_layout_equals_fresh_instrumented_run() {
    let Some((tl, root, project)) = setup("t2", 2, Variant::Pure) else {
        return;
    };
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debounce = Duration::from_millis(50);
    let s = Session::open(cfg).unwrap();
    let Event::LayoutUpdate {
        eligible_paragraphs,
        ..
    } = wait_layout(&s, 120)
    else {
        unreachable!()
    };
    // edits: one fast-path paragraph, one label/ref pair (needs two passes), one new paragraph
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans
        .iter()
        .find(|sp| eligible_paragraphs.contains(&sp.id))
        .unwrap();
    let p1 = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    s.apply_edit(
        "main.tex",
        Edit {
            start_byte: p1,
            end_byte: p1,
            text: " inserted words that lengthen the paragraph by a line or so, hopefully".into(),
        },
    )
    .unwrap();
    let doc = s.document_text("main.tex").unwrap();
    let last_body = spans
        .iter()
        .rev()
        .find(|sp| eligible_paragraphs.contains(&sp.id))
        .unwrap();
    let p2 = doc[..last_body.range.end].rfind('.').unwrap() + 1;
    s.apply_edit(
        "main.tex",
        Edit {
            start_byte: p2,
            end_byte: p2,
            text: "\\label{sec:x} As seen on page~\\pageref{sec:x}.\n\nA brand new paragraph."
                .into(),
        },
    )
    .unwrap();
    // wait until converged on the latest revision
    let mut converged = None;
    for _ in 0..6 {
        let Event::LayoutUpdate {
            versions,
            convergence,
            compile,
            ..
        } = wait_layout(&s, 180)
        else {
            unreachable!()
        };
        assert_eq!(compile, CompileStatus::Ok);
        if convergence == Convergence::Converged
            && versions.source_revision == s.versions().source_revision
        {
            converged = Some(versions);
            break;
        }
    }
    let versions = converged.expect("converged");
    // fresh instrumented run of the final text in a separate directory
    let final_text = s.document_text("main.tex").unwrap();
    let fresh_src = root.join("fresh-src");
    std::fs::create_dir_all(&fresh_src).unwrap();
    std::fs::write(fresh_src.join("main.tex"), &final_text).unwrap();
    let fresh = run_pass(
        &tl,
        &fresh_src,
        "main.tex",
        &root.join("fresh-out"),
        5,
        BibTool::Auto,
        true,
    )
    .unwrap();
    assert!(fresh.aux_stable);
    // compare page display lists and per-paragraph placements with the session's layout
    let session_out = root.join("build").join("bg");
    for n in 1..=fresh.capture.json.pages {
        let a =
            std::fs::read_to_string(session_out.join(format!("main.rtex-page{n}.json"))).unwrap();
        let b = std::fs::read_to_string(
            root.join("fresh-out")
                .join(format!("main.rtex-page{n}.json")),
        )
        .unwrap();
        assert_eq!(
            a, b,
            "page {n} display list differs between converged session layout and fresh run"
        );
    }
    let a: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session_out.join("main.rtex.json")).unwrap())
            .unwrap();
    let b: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("fresh-out").join("main.rtex.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(a["paragraphs"], b["paragraphs"]);
    assert!(versions.layout_version >= 2);
    // and the PDFs are equal (instrumented vs instrumented, different runs)
    let d = rtex_verify_compare(
        &session_out.join("main.pdf"),
        &root.join("fresh-out").join("main.pdf"),
    );
    assert!(d, "PDFs differ");
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}

fn rtex_verify_compare(a: &std::path::Path, b: &std::path::Path) -> bool {
    // byte-level comparison is enough here: fixtures suppress all optional PDF info
    std::fs::read(a).unwrap() == std::fs::read(b).unwrap()
}

#[test]
fn capture_package_does_not_change_output() {
    let Some((tl, root, project)) = setup("t3", 2, Variant::Mixed) else {
        return;
    };
    let inst = run_pass(
        &tl,
        &project,
        "main.tex",
        &root.join("inst"),
        5,
        BibTool::Auto,
        true,
    )
    .unwrap();
    let clean = run_pass(
        &tl,
        &project,
        "main.tex",
        &root.join("clean"),
        5,
        BibTool::Auto,
        false,
    )
    .unwrap();
    assert!(inst.aux_stable && clean.aux_stable);
    assert!(inst.bib_ran, "mixed fixture cites, biber must run");
    assert_eq!(
        std::fs::read(&inst.capture.pdf).unwrap(),
        std::fs::read(&clean.capture.pdf).unwrap(),
        "instrumented and clean PDFs differ"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mixed_document_converges_through_biber_and_degrades_literal_pages() {
    let Some((_tl, root, project)) = setup("mixed", 2, Variant::Mixed) else {
        return;
    };
    // add a paragraph with a raw PDF literal: the traversal cannot represent its drawing
    let main = project.join("main.tex");
    let text = std::fs::read_to_string(&main).unwrap();
    let text = text.replacen("\\end{document}", "A paragraph with a literal \\pdfextension literal{0 0 10 10 re f} drawing.\n\n\\end{document}", 1);
    std::fs::write(&main, text).unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    let s = Session::open(cfg).unwrap();
    let Event::LayoutUpdate {
        convergence,
        compile,
        passes,
        pages_changed,
        pdf_fallback,
        ..
    } = wait_layout(&s, 300)
    else {
        unreachable!()
    };
    assert_eq!(compile, CompileStatus::Ok);
    assert_eq!(convergence, Convergence::Converged);
    assert!(
        passes >= 2,
        "citations need at least two passes, got {passes}"
    );
    let degraded: Vec<i64> = pages_changed
        .iter()
        .filter(|p| !p.exact)
        .map(|p| p.page)
        .collect();
    assert!(!degraded.is_empty(), "the literal page must be degraded");
    assert!(
        pdf_fallback.as_ref().map(|p| p.exists()).unwrap_or(false),
        "degraded pages need a PDF fallback"
    );
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}
