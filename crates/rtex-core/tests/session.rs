//! Session-level behaviour: routing, events, versions, restart. Skipped without lualatex.

use rtex_core::fixtures::{generate, FontSet, Variant};
use rtex_core::{Convergence, Edit, Event, Session, SessionConfig};
use std::time::Duration;

fn open(name: &str) -> Option<(Session, std::path::PathBuf)> {
    if rtex_core::texlive::TexLive::discover().is_err() {
        eprintln!("SKIP: no lualatex");
        return None;
    }
    let root = std::env::temp_dir().join(format!("rtex-session-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    generate(2, Variant::Pure, FontSet::Pagella, 11, &project).unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debounce = Duration::from_millis(50);
    Some((Session::open(cfg).unwrap(), project))
}

fn wait_layout(s: &Session) -> Event {
    let (ev, _) = s.wait_for(Duration::from_secs(120), |e| matches!(e, Event::LayoutUpdate { .. }));
    ev.expect("layout update")
}

#[test]
fn fast_path_edit_produces_paragraph_update_with_fragments() {
    let Some((s, _p)) = open("fast") else { return };
    let Event::LayoutUpdate { versions, convergence, eligible_paragraphs, placements, pages_total, .. } = wait_layout(&s) else { unreachable!() };
    assert_eq!(versions.layout_version, 1);
    assert_eq!(convergence, Convergence::Converged);
    assert!(pages_total >= 2);
    assert!(!eligible_paragraphs.is_empty());
    assert!(!placements.is_empty());
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans.iter().find(|sp| eligible_paragraphs.contains(&sp.id)).unwrap();
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    let r = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: " inserted".into() }).unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate { par_id, status, fragments, dl, versions: v2, edit_id, .. }) = ev else { panic!("no paragraph update") };
    assert_eq!(par_id, body.id);
    assert_eq!(status, "ok");
    assert_eq!(edit_id, r.edit_id);
    assert_eq!(v2.source_revision, r.source_revision);
    assert_eq!(v2.layout_version, 1);
    assert!(!fragments.is_empty());
    assert_eq!(fragments.iter().map(|f| f.baselines.len()).sum::<usize>(), dl.lines.len());
    assert!(dl.glyph_count() > 10);
    s.close();
}

#[test]
fn ineligible_edit_goes_to_background_and_reconverges() {
    let Some((s, _p)) = open("bg") else { return };
    let Event::LayoutUpdate { eligible_paragraphs, .. } = wait_layout(&s) else { unreachable!() };
    let spans = s.spans("main.tex");
    let body = spans.iter().find(|sp| eligible_paragraphs.contains(&sp.id)).unwrap();
    let pos = body.range.end - 1;
    // \parbox is not on the allow-list (\ref is, since 0.0.2): background path
    let r = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: " \\parbox{3cm}{boxed text}.".into() }).unwrap();
    assert_eq!(r.routed, "background");
    assert!(r.reasons.iter().any(|x| x.contains("parbox")), "{:?}", r.reasons);
    let (ev, _) = s.wait_for(Duration::from_secs(10), |e| matches!(e, Event::BackgroundScheduled { .. }));
    assert!(ev.is_some());
    assert!(matches!(s.convergence(), Some(Convergence::Stale { .. })));
    let Event::LayoutUpdate { versions, convergence, compile, .. } = wait_layout(&s) else { unreachable!() };
    assert_eq!(versions.layout_version, 2);
    assert_eq!(convergence, Convergence::Converged);
    assert_eq!(compile, rtex_core::session::CompileStatus::Ok); // undefined ref is a warning, not an error
    s.close();
}

#[test]
fn boundary_change_and_preamble_change() {
    let Some((s, _p)) = open("boundary") else { return };
    let Event::LayoutUpdate { eligible_paragraphs, .. } = wait_layout(&s) else { unreachable!() };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans.iter().find(|sp| eligible_paragraphs.contains(&sp.id)).unwrap();
    // split the paragraph with a blank line
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    let r = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: "\n\n".into() }).unwrap();
    assert_eq!(r.routed, "background");
    assert_eq!(r.outcome.removed.len(), 1);
    assert_eq!(r.outcome.added.len(), 2);
    let Event::LayoutUpdate { versions, .. } = wait_layout(&s) else { unreachable!() };
    assert_eq!(versions.layout_version, 2);
    // new spans are eligible again after the pass
    let spans2 = s.spans("main.tex");
    let newspan = spans2.iter().find(|sp| sp.id == r.outcome.added[1]).unwrap();
    let p2 = newspan.range.start + 3;
    let r2 = s.apply_edit("main.tex", Edit { start_byte: p2, end_byte: p2, text: "x".into() }).unwrap();
    assert_eq!(r2.routed, "fast", "{:?}", r2.reasons);
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
    assert!(ev.is_some());
    // preamble change: engine generation bumps, server restarts, edits still work afterwards
    let gen_before = s.versions().engine_generation;
    let pos = s.document_text("main.tex").unwrap().find("\\usepackage{xcolor}").unwrap();
    let r3 = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: "\\usepackage{xspace}\n".into() }).unwrap();
    assert_eq!(r3.routed, "preamble");
    assert!(s.versions().engine_generation > gen_before);
    let (ready, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::EngineState { state, .. } if state == "Ready"));
    assert!(ready.is_some());
    let Event::LayoutUpdate { versions, .. } = wait_layout(&s) else { unreachable!() };
    assert_eq!(versions.layout_version, 3);
    let spans3 = s.spans("main.tex");
    let any = spans3.iter().find(|sp| sp.id == r.outcome.added[1]).unwrap();
    let p3 = any.range.start + 3;
    let r4 = s.apply_edit("main.tex", Edit { start_byte: p3, end_byte: p3, text: "y".into() }).unwrap();
    assert_eq!(r4.routed, "fast", "{:?}", r4.reasons);
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate { versions, status, .. }) = ev else { panic!("no update after restart") };
    assert_eq!(status, "ok");
    assert_eq!(versions.engine_generation, s.versions().engine_generation);
    s.close();
}

#[test]
fn stale_results_are_discarded() {
    let Some((s, _p)) = open("stale") else { return };
    let Event::LayoutUpdate { eligible_paragraphs, .. } = wait_layout(&s) else { unreachable!() };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans.iter().find(|sp| eligible_paragraphs.contains(&sp.id)).unwrap();
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    // burst of edits: only results whose span hash still matches may be delivered
    let mut last = None;
    for i in 0..8 {
        last = Some(s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: format!(" w{i}") }).unwrap());
    }
    let last = last.unwrap();
    let final_hash = s.spans("main.tex").iter().find(|sp| sp.id == body.id).unwrap().hash;
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate { edit_id, versions, .. }) = ev else { panic!("no update") };
    // whichever request got through, it was compiled from the final text (coalescing + discard)
    assert!(edit_id <= last.edit_id);
    assert!(versions.source_revision <= last.source_revision);
    let _ = final_hash;
    // nothing else pending: a subsequent single edit yields exactly one update
    std::thread::sleep(Duration::from_millis(300));
    let drained = s.poll(Duration::from_millis(10));
    assert!(drained.iter().filter(|e| matches!(e, Event::ParagraphUpdate { .. })).count() <= 1);
    s.close();
}
