//! Exported PDFs equal an independent clean LuaLaTeX build (byte-exact for the fixtures, which
//! suppress optional PDF info), and the export status is honest. Skipped without lualatex.

use lode_core::background::{run_pass, BibTool};
use lode_core::fixtures::{generate, FontSet, Variant};
use lode_core::session::CompileStatus;
use lode_core::texlive::TexLive;
use lode_core::{Edit, Event, Session, SessionConfig};
use std::time::Duration;

fn setup(name: &str, variant: Variant) -> Option<(TexLive, std::path::PathBuf, std::path::PathBuf)> {
    let tl = match TexLive::discover() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SKIP: {e}");
            return None;
        }
    };
    let root = std::env::temp_dir().join(format!("lode-export-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    generate(2, variant, FontSet::Pagella, 3, &project).unwrap();
    Some((tl, root, project))
}

#[test]
fn export_equals_clean_build_after_edits() {
    let Some((tl, root, project)) = setup("edits", Variant::Mixed) else { return };
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    let s = Session::open(cfg).unwrap();
    let (first, _) = s.wait_for(Duration::from_secs(300), |e| matches!(e, Event::LayoutUpdate { .. }));
    let Some(Event::LayoutUpdate { eligible_paragraphs, .. }) = first else { panic!("no layout") };
    // edit the buffer (the on-disk file is untouched: the export must use the buffer)
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans.iter().find(|sp| eligible_paragraphs.contains(&sp.id)).unwrap();
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: " exported-edit".into() }).unwrap();
    let out = root.join("exported.pdf");
    let job = s.export_pdf(&out);
    let (ev, _) = s.wait_for(Duration::from_secs(600), |e| matches!(e, Event::PdfExported { job_id, .. } if *job_id == job));
    let Some(Event::PdfExported { path, status, converged, passes, .. }) = ev else { panic!("no export") };
    assert_eq!(status, CompileStatus::Ok);
    assert!(converged);
    assert!(passes >= 2, "mixed document with citations needs >= 2 passes");
    assert_eq!(path.as_deref(), Some(out.as_path()));
    // independent clean build of the edited text
    let clean_src = root.join("clean-src");
    std::fs::create_dir_all(&clean_src).unwrap();
    for entry in std::fs::read_dir(&project).unwrap() {
        let e = entry.unwrap();
        if e.path().is_file() {
            std::fs::copy(e.path(), clean_src.join(e.file_name())).unwrap();
        }
    }
    std::fs::write(clean_src.join("main.tex"), s.document_text("main.tex").unwrap()).unwrap();
    let o = run_pass(&tl, &clean_src, "main.tex", &root.join("clean-out"), 5, BibTool::Auto, false).unwrap();
    assert!(o.aux_stable);
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&o.capture.pdf).unwrap(), "exported PDF differs from the clean build");
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_reports_errors_honestly() {
    let Some((_tl, root, project)) = setup("errors", Variant::Pure) else { return };
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    let s = Session::open(cfg).unwrap();
    let (first, _) = s.wait_for(Duration::from_secs(300), |e| matches!(e, Event::LayoutUpdate { .. }));
    assert!(first.is_some());
    let doc = s.document_text("main.tex").unwrap();
    let pos = doc.find("\\end{document}").unwrap();
    s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: "Broken \\nosuchmacro here.\n\n".into() }).unwrap();
    let out = root.join("broken.pdf");
    let job = s.export_pdf(&out);
    let (ev, _) = s.wait_for(Duration::from_secs(600), |e| matches!(e, Event::PdfExported { job_id, .. } if *job_id == job));
    let Some(Event::PdfExported { status, converged, .. }) = ev else { panic!("no export") };
    assert!(matches!(status, CompileStatus::CompiledWithErrors { .. }), "{status:?}");
    assert!(!converged, "an export with errors must not claim convergence");
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}
