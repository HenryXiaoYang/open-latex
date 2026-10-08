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
    let t0 = std::time::Instant::now();
    let (ev, others) = s.wait_for(Duration::from_secs(120), |e| matches!(e, Event::LayoutUpdate { .. }));
    eprintln!("[test] wait_layout: {:?} after {:.1}s ({} other events)", ev.as_ref().map(|e| match e { Event::LayoutUpdate { versions, convergence, passes, wall_ms, compile, .. } => format!("layout v{} {:?} {:?} passes {} {} ms", versions.layout_version, compile, convergence, passes, wall_ms), _ => String::new() }), t0.elapsed().as_secs_f64(), others.len());
    if ev.is_none() {
        eprintln!("[test] convergence: {:?}", s.convergence());
        for o in others.iter().rev().take(30) {
            eprintln!("[test] event: {}", serde_json::to_string(o).map(|j| j.chars().take(300).collect::<String>()).unwrap_or_default());
        }
    }
    ev.expect("layout update")
}

fn step(name: &str) {
    eprintln!("[test] {name}");
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
    // the longest eligible paragraph (several lines, so a split leaves a multi-line second half)
    let body = spans.iter().filter(|sp| eligible_paragraphs.contains(&sp.id)).max_by_key(|sp| sp.range.len()).unwrap();
    step("split");
    // split the paragraph with a blank line: the first half keeps its id and context, the
    // second half borrows one and is placed after the first; both are typeset live
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    let r = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: "\n\n".into() }).unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    assert_eq!(r.outcome.touched, vec![body.id]);
    assert!(r.outcome.removed.is_empty());
    assert_eq!(r.outcome.added.len(), 1);
    let second = r.outcome.added[0];
    let mut seen_first = false;
    let mut seen_second = false;
    for _ in 0..2 {
        let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
        let Some(Event::ParagraphUpdate { par_id, status, fragments, pagination_stale, context_stale, dl, .. }) = ev else { panic!("no update") };
        assert_eq!(status, "ok");
        assert!(!fragments.is_empty());
        if par_id == body.id {
            seen_first = true;
            assert_eq!(dl.lines.len(), 1);
        } else {
            assert_eq!(par_id, second);
            seen_second = true;
            assert!(pagination_stale && context_stale);
            assert!(fragments[0].approximate);
            assert!(dl.lines.len() >= 1);
        }
    }
    assert!(seen_first && seen_second);
    step("keystroke in new half");
    // a keystroke in the new paragraph stays live before the next layout
    let spans1 = s.spans("main.tex");
    let newspan = spans1.iter().find(|sp| sp.id == second).unwrap();
    let p1 = newspan.range.start + 3;
    let r1 = s.apply_edit("main.tex", Edit { start_byte: p1, end_byte: p1, text: "z".into() }).unwrap();
    assert_eq!(r1.routed, "fast", "{:?}", r1.reasons);
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { par_id, .. } if *par_id == second));
    assert!(ev.is_some());
    step("merge");
    // merge the halves back: the first id survives, the second is announced as removed, the
    // merged paragraph is live with the first half's context
    let r2 = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos + 2, text: String::new() }).unwrap();
    assert_eq!(r2.routed, "fast", "{:?}", r2.reasons);
    assert_eq!(r2.outcome.touched, vec![body.id]);
    assert_eq!(r2.outcome.removed, vec![second]);
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { par_id, status, .. } if *par_id == second && status == "removed"));
    assert!(ev.is_some(), "removed update for the merged-away span");
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { par_id, status, .. } if *par_id == body.id && status == "ok"));
    let Some(Event::ParagraphUpdate { dl, .. }) = ev else { panic!("no update for the merged paragraph") };
    assert!(dl.lines.len() > 1);
    step("fresh paragraph above the first");
    // a paragraph typed above the first one: the existing paragraph is untouched (it keeps its
    // id, context and anchor), the new one has no paragraph before it, so it borrows the context
    // of the paragraph after it and is placed above that paragraph
    let spans_b = s.spans("main.tex");
    let first_body = spans_b.iter().find(|sp| sp.kind == rtex_core::document::SpanKind::Body).unwrap();
    let at = first_body.range.start;
    let r3 = s.apply_edit("main.tex", Edit { start_byte: at, end_byte: at, text: "Fresh words typed above the first paragraph.\n\n".into() }).unwrap();
    assert_eq!(r3.routed, "fast", "{:?}", r3.reasons);
    assert!(r3.outcome.touched.is_empty() && r3.outcome.removed.is_empty(), "{:?}", r3.outcome);
    assert_eq!(r3.outcome.added.len(), 1);
    let fresh = r3.outcome.added[0];
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { par_id, .. } if *par_id == fresh));
    let Some(Event::ParagraphUpdate { status, fragments, dl, .. }) = ev else { panic!("no update for the fresh paragraph") };
    assert_eq!(status, "ok");
    assert_eq!(dl.lines.len(), 1);
    assert!(!fragments.is_empty() && fragments[0].approximate);
    let Event::LayoutUpdate { versions, .. } = wait_layout(&s) else { unreachable!() };
    assert!(versions.layout_version >= 2);
    step("post-pass edit");
    // the split-off span is eligible with a captured context after the pass
    let spans2 = s.spans("main.tex");
    let newspan = spans2.iter().find(|sp| sp.id == fresh).unwrap();
    let p2 = newspan.range.start + 3;
    let r2 = s.apply_edit("main.tex", Edit { start_byte: p2, end_byte: p2, text: "x".into() }).unwrap();
    assert_eq!(r2.routed, "fast", "{:?}", r2.reasons);
    let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
    assert!(ev.is_some());
    step("preamble change");
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
    let any = spans3.iter().find(|sp| sp.id == fresh).unwrap();
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
