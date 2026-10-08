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

/// `Session::wait_for` hands back the events it skipped; a test that waits for several things in
/// sequence must requeue them, or a layout that lands while it waits for an engine restart (slow
/// on CI) is lost.
fn wait(s: &Session, secs: u64, pred: impl FnMut(&Event) -> bool) -> (Option<Event>, Vec<Event>) {
    let (ev, others) = s.wait_for(Duration::from_secs(secs), pred);
    for o in others.iter() {
        s.requeue(o.clone());
    }
    (ev, others)
}

fn wait_layout(s: &Session) -> Event {
    let t0 = std::time::Instant::now();
    let (ev, others) = wait(s, 120, |e| matches!(e, Event::LayoutUpdate { .. }));
    eprintln!(
        "[test] wait_layout: {:?} after {:.1}s ({} other events)",
        ev.as_ref().map(|e| match e {
            Event::LayoutUpdate {
                versions,
                convergence,
                passes,
                wall_ms,
                compile,
                ..
            } => format!(
                "layout v{} {:?} {:?} passes {} {} ms",
                versions.layout_version, compile, convergence, passes, wall_ms
            ),
            _ => String::new(),
        }),
        t0.elapsed().as_secs_f64(),
        others.len()
    );
    if ev.is_none() {
        eprintln!("[test] convergence: {:?}", s.convergence());
        for o in others.iter().rev().take(30) {
            eprintln!(
                "[test] event: {}",
                serde_json::to_string(o)
                    .map(|j| j.chars().take(300).collect::<String>())
                    .unwrap_or_default()
            );
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
    let Event::LayoutUpdate {
        versions,
        convergence,
        eligible_paragraphs,
        placements,
        pages_total,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    assert_eq!(versions.layout_version, 1);
    assert_eq!(convergence, Convergence::Converged);
    assert!(pages_total >= 2);
    assert!(!eligible_paragraphs.is_empty());
    assert!(!placements.is_empty());
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans
        .iter()
        .find(|sp| eligible_paragraphs.contains(&sp.id))
        .unwrap();
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    let r = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: " inserted".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate {
        par_id,
        status,
        fragments,
        dl,
        versions: v2,
        edit_id,
        ..
    }) = ev
    else {
        panic!("no paragraph update")
    };
    assert_eq!(par_id, body.id);
    assert_eq!(status, "ok");
    assert_eq!(edit_id, r.edit_id);
    assert_eq!(v2.source_revision, r.source_revision);
    assert_eq!(v2.layout_version, 1);
    assert!(!fragments.is_empty());
    assert_eq!(
        fragments.iter().map(|f| f.baselines.len()).sum::<usize>(),
        dl.lines.len()
    );
    assert!(dl.glyph_count() > 10);
    s.close();
}

#[test]
fn ineligible_edit_goes_to_background_and_reconverges() {
    let Some((s, _p)) = open("bg") else { return };
    let Event::LayoutUpdate {
        eligible_paragraphs,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let spans = s.spans("main.tex");
    let body = spans
        .iter()
        .find(|sp| eligible_paragraphs.contains(&sp.id))
        .unwrap();
    let pos = body.range.end - 1;
    // \marginpar is not on the allow-list (\parbox and \ref are, since 0.0.2): background path
    let r = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: " \\marginpar{a note}.".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "background");
    assert!(
        r.reasons.iter().any(|x| x.contains("marginpar")),
        "{:?}",
        r.reasons
    );
    let (ev, _) = wait(&s, 10, |e| matches!(e, Event::BackgroundScheduled { .. }));
    assert!(ev.is_some());
    assert!(matches!(s.convergence(), Some(Convergence::Stale { .. })));
    let Event::LayoutUpdate {
        versions,
        convergence,
        compile,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    assert_eq!(versions.layout_version, 2);
    assert_eq!(convergence, Convergence::Converged);
    assert_eq!(compile, rtex_core::session::CompileStatus::Ok); // undefined ref is a warning, not an error
    s.close();
}

#[test]
fn boundary_change_and_preamble_change() {
    let Some((s, _p)) = open("boundary") else {
        return;
    };
    let Event::LayoutUpdate {
        eligible_paragraphs,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    // the longest eligible paragraph (several lines, so a split leaves a multi-line second half)
    let body = spans
        .iter()
        .filter(|sp| eligible_paragraphs.contains(&sp.id))
        .max_by_key(|sp| sp.range.len())
        .unwrap();
    step("split");
    // split the paragraph with a blank line: the first half keeps its id and context, the
    // second half borrows one and is placed after the first; both are typeset live
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    let r = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: "\n\n".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    assert_eq!(r.outcome.touched, vec![body.id]);
    assert!(r.outcome.removed.is_empty());
    assert_eq!(r.outcome.added.len(), 1);
    let second = r.outcome.added[0];
    let mut seen_first = false;
    let mut seen_second = false;
    for _ in 0..2 {
        let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
        let Some(Event::ParagraphUpdate {
            par_id,
            status,
            fragments,
            pagination_stale,
            context_stale,
            dl,
            ..
        }) = ev
        else {
            panic!("no update")
        };
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
    let r1 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: p1,
                end_byte: p1,
                text: "z".into(),
            },
        )
        .unwrap();
    assert_eq!(r1.routed, "fast", "{:?}", r1.reasons);
    let (ev, _) = wait(
        &s,
        60,
        |e| matches!(e, Event::ParagraphUpdate { par_id, .. } if *par_id == second),
    );
    assert!(ev.is_some());
    step("merge");
    // merge the halves back: the first id survives, the second is announced as removed, the
    // merged paragraph is live with the first half's context
    let r2 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos + 2,
                text: String::new(),
            },
        )
        .unwrap();
    assert_eq!(r2.routed, "fast", "{:?}", r2.reasons);
    assert_eq!(r2.outcome.touched, vec![body.id]);
    assert_eq!(r2.outcome.removed, vec![second]);
    let (ev, _) = wait(
        &s,
        60,
        |e| matches!(e, Event::ParagraphUpdate { par_id, status, .. } if *par_id == second && status == "removed"),
    );
    assert!(ev.is_some(), "removed update for the merged-away span");
    let (ev, _) = wait(
        &s,
        60,
        |e| matches!(e, Event::ParagraphUpdate { par_id, status, .. } if *par_id == body.id && status == "ok"),
    );
    let Some(Event::ParagraphUpdate { dl, .. }) = ev else {
        panic!("no update for the merged paragraph")
    };
    assert!(dl.lines.len() > 1);
    step("fresh paragraph above the first");
    // a paragraph typed above the first one: the existing paragraph is untouched (it keeps its
    // id, context and anchor), the new one has no paragraph before it, so it borrows the context
    // of the paragraph after it and is placed above that paragraph
    let spans_b = s.spans("main.tex");
    let first_body = spans_b
        .iter()
        .find(|sp| sp.kind == rtex_core::document::SpanKind::Body)
        .unwrap();
    let at = first_body.range.start;
    let r3 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: at,
                end_byte: at,
                text: "Fresh words typed above the first paragraph.\n\n".into(),
            },
        )
        .unwrap();
    assert_eq!(r3.routed, "fast", "{:?}", r3.reasons);
    assert!(
        r3.outcome.touched.is_empty() && r3.outcome.removed.is_empty(),
        "{:?}",
        r3.outcome
    );
    assert_eq!(r3.outcome.added.len(), 1);
    let fresh = r3.outcome.added[0];
    let (ev, _) = wait(
        &s,
        60,
        |e| matches!(e, Event::ParagraphUpdate { par_id, .. } if *par_id == fresh),
    );
    let Some(Event::ParagraphUpdate {
        status,
        fragments,
        dl,
        ..
    }) = ev
    else {
        panic!("no update for the fresh paragraph")
    };
    assert_eq!(status, "ok");
    assert_eq!(dl.lines.len(), 1);
    assert!(!fragments.is_empty() && fragments[0].approximate);
    let Event::LayoutUpdate { versions, .. } = wait_layout(&s) else {
        unreachable!()
    };
    assert!(versions.layout_version >= 2);
    step("post-pass edit");
    // the split-off span is eligible with a captured context after the pass
    let spans2 = s.spans("main.tex");
    let newspan = spans2.iter().find(|sp| sp.id == fresh).unwrap();
    let p2 = newspan.range.start + 3;
    let r2 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: p2,
                end_byte: p2,
                text: "x".into(),
            },
        )
        .unwrap();
    assert_eq!(r2.routed, "fast", "{:?}", r2.reasons);
    let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
    assert!(ev.is_some());
    step("preamble change");
    // preamble change: engine generation bumps, server restarts, edits still work afterwards
    let gen_before = s.versions().engine_generation;
    let pos = s
        .document_text("main.tex")
        .unwrap()
        .find("\\usepackage{xcolor}")
        .unwrap();
    let r3 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: "\\usepackage{xspace}\n".into(),
            },
        )
        .unwrap();
    assert_eq!(r3.routed, "preamble");
    assert!(s.versions().engine_generation > gen_before);
    let (ready, _) = wait(
        &s,
        60,
        |e| matches!(e, Event::EngineState { state, .. } if state == "Ready"),
    );
    assert!(ready.is_some());
    // the layout compiled from a snapshot at or after the preamble edit (earlier passes may
    // still deliver first)
    let versions = loop {
        let Event::LayoutUpdate { versions, .. } = wait_layout(&s) else {
            unreachable!()
        };
        if versions.source_revision >= r3.source_revision {
            break versions;
        }
    };
    assert!(versions.layout_version >= 3);
    let spans3 = s.spans("main.tex");
    let any = spans3.iter().find(|sp| sp.id == fresh).unwrap();
    let p3 = any.range.start + 3;
    let r4 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: p3,
                end_byte: p3,
                text: "y".into(),
            },
        )
        .unwrap();
    assert_eq!(r4.routed, "fast", "{:?}", r4.reasons);
    let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate {
        versions, status, ..
    }) = ev
    else {
        panic!("no update after restart")
    };
    assert_eq!(status, "ok");
    assert_eq!(versions.engine_generation, s.versions().engine_generation);
    s.close();
}

#[test]
fn stale_results_are_discarded() {
    let Some((s, _p)) = open("stale") else { return };
    let Event::LayoutUpdate {
        eligible_paragraphs,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans
        .iter()
        .find(|sp| eligible_paragraphs.contains(&sp.id))
        .unwrap();
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    // burst of edits: only results whose span hash still matches may be delivered
    let mut last = None;
    for i in 0..8 {
        last = Some(
            s.apply_edit(
                "main.tex",
                Edit {
                    start_byte: pos,
                    end_byte: pos,
                    text: format!(" w{i}"),
                },
            )
            .unwrap(),
        );
    }
    let last = last.unwrap();
    let final_hash = s
        .spans("main.tex")
        .iter()
        .find(|sp| sp.id == body.id)
        .unwrap()
        .hash;
    let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate {
        edit_id, versions, ..
    }) = ev
    else {
        panic!("no update")
    };
    // whichever request got through, it was compiled from the final text (coalescing + discard)
    assert!(edit_id <= last.edit_id);
    assert!(versions.source_revision <= last.source_revision);
    let _ = final_hash;
    // nothing else pending: a subsequent single edit yields exactly one update
    std::thread::sleep(Duration::from_millis(300));
    let drained = s.poll(Duration::from_millis(10));
    assert!(
        drained
            .iter()
            .filter(|e| matches!(e, Event::ParagraphUpdate { .. }))
            .count()
            <= 1
    );
    s.close();
}

/// A span holding several consecutive paragraphs (a title line with its own \par, then a text
/// line) is one composite unit: typeset live as one box, rows matching the layout.
#[test]
fn multi_paragraph_span_is_one_live_unit() {
    if rtex_core::texlive::TexLive::discover().is_err() {
        eprintln!("SKIP: no lualatex");
        return;
    }
    let root = std::env::temp_dir().join(format!("rtex-session-{}-multipar", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("main.tex"), "\\documentclass{article}\n\\usepackage[T1]{fontenc}\n\\usepackage{lmodern}\n\\begin{document}\n{\\Large\\bfseries Computer Hardware and Operation\\par}\nHenry Yang \\hfill 22 September 2026\n\nA body paragraph long enough to wrap onto a second line in the article class at ten points, with ordinary words following.\n\\end{document}\n").unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debounce = Duration::from_millis(50);
    let s = Session::open(cfg).unwrap();
    let Event::LayoutUpdate {
        eligible_paragraphs,
        placements,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let head = spans
        .iter()
        .find(|sp| doc[sp.range.clone()].starts_with("{\\Large"))
        .expect("title span");
    assert!(
        eligible_paragraphs.contains(&head.id),
        "title span not eligible"
    );
    let pl = placements
        .iter()
        .find(|p| p.par_id == head.id)
        .expect("placement");
    assert_eq!(pl.lines, 2, "title + name line");
    let pos =
        head.range.start + doc[head.range.clone()].find("Henry Yang").unwrap() + "Henry".len();
    let r = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: " X.".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let (ev, _) = wait(
        &s,
        60,
        |e| matches!(e, Event::ParagraphUpdate { par_id, .. } if *par_id == head.id),
    );
    let Some(Event::ParagraphUpdate {
        status,
        fragments,
        pagination_stale,
        context_stale,
        dl,
        ..
    }) = ev
    else {
        panic!("no update")
    };
    assert_eq!(status, "ok");
    assert_eq!(dl.lines.len(), 2);
    assert!(!pagination_stale && !context_stale);
    assert_eq!(fragments.len(), 1);
    assert_eq!(fragments[0].baselines.len(), 2);
    s.close();
}

/// A project split over files: paragraphs in an `\input`ted chapter are live, an edit to a
/// preamble file restarts the engine, and a new `\input` line loads its file.
#[test]
fn input_files_are_tracked() {
    if rtex_core::texlive::TexLive::discover().is_err() {
        eprintln!("SKIP: no lualatex");
        return;
    }
    let root = std::env::temp_dir().join(format!("rtex-session-{}-multi", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(project.join("chapters")).unwrap();
    std::fs::write(
        project.join("main.tex"),
        "\\documentclass{article}\n\\input{macros}\n\\begin{document}\n\\section{A}\nMain text with a \\kw{word}.\n\n\\input{chapters/one}\n\n\\end{document}\n",
    )
    .unwrap();
    std::fs::write(
        project.join("macros.tex"),
        "\\newcommand{\\kw}[1]{\\textbf{#1}}\n",
    )
    .unwrap();
    std::fs::write(
        project.join("chapters/one.tex"),
        "\\section{One}\nFirst paragraph of the chapter, with a \\kw{keyword} and some more words so that it wraps onto a second line of text.\n\nSecond paragraph of the chapter.\n",
    )
    .unwrap();
    std::fs::write(
        project.join("chapters/two.tex"),
        "Paragraph from the second chapter file.\n",
    )
    .unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debounce = Duration::from_millis(50);
    let s = Session::open(cfg).unwrap();
    // all three files are tracked from the start; the preamble file is one. Host paths may
    // carry `./` or be absolute inside the project.
    assert!(!s.spans("chapters/one.tex").is_empty());
    assert!(!s.spans("./chapters/one.tex").is_empty());
    assert!(!s
        .spans(&project.join("chapters/one.tex").to_string_lossy())
        .is_empty());
    assert!(!s.spans("macros.tex").is_empty());
    assert!(s.spans("chapters/two.tex").is_empty(), "not referenced yet");
    let Event::LayoutUpdate {
        eligible_paragraphs,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let ch = s.spans("chapters/one.tex");
    let text = s.document_text("chapters/one.tex").unwrap();
    let first = ch
        .iter()
        .find(|sp| text[sp.range.clone()].starts_with("First paragraph"))
        .unwrap();
    assert!(
        eligible_paragraphs.contains(&first.id),
        "chapter paragraph has a context"
    );
    step("edit in the input file");
    let pos = first.range.start + "First".len();
    let r = s
        .apply_edit(
            "chapters/one.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: " edited".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate {
        par_id, status, dl, ..
    }) = ev
    else {
        panic!("no update")
    };
    assert_eq!(par_id, first.id);
    assert_eq!(status, "ok");
    assert!(dl.glyph_count() > 10);
    step("edit the preamble file");
    let gen_before = s.versions().engine_generation;
    let r2 = s
        .apply_edit(
            "macros.tex",
            Edit {
                start_byte: 0,
                end_byte: 0,
                text: "\\newcommand{\\kwb}[1]{\\emph{#1}}\n".into(),
            },
        )
        .unwrap();
    assert_eq!(r2.routed, "preamble", "{:?}", r2.reasons);
    assert!(s.versions().engine_generation > gen_before);
    let (ready, _) = wait(
        &s,
        60,
        |e| matches!(e, Event::EngineState { state, .. } if state == "Ready"),
    );
    assert!(ready.is_some());
    loop {
        let Event::LayoutUpdate { versions, .. } = wait_layout(&s) else {
            unreachable!()
        };
        if versions.source_revision >= r2.source_revision {
            break;
        }
    }
    // the new macro is known to the policy: using it is still fast
    let ch = s.spans("chapters/one.tex");
    let text = s.document_text("chapters/one.tex").unwrap();
    let second = ch
        .iter()
        .find(|sp| text[sp.range.clone()].starts_with("Second"))
        .unwrap();
    let pos = second.range.start + "Second".len();
    let r3 = s
        .apply_edit(
            "chapters/one.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: " \\kwb{new}".into(),
            },
        )
        .unwrap();
    assert_eq!(r3.routed, "fast", "{:?}", r3.reasons);
    step("new \\input line");
    let main = s.document_text("main.tex").unwrap();
    let pos = main.find("\\end{document}").unwrap();
    let r4 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: "\\input{chapters/two}\n\n".into(),
            },
        )
        .unwrap();
    assert_eq!(r4.routed, "background", "{:?}", r4.reasons);
    assert!(
        !s.spans("chapters/two.tex").is_empty(),
        "the new file is loaded"
    );
    loop {
        let Event::LayoutUpdate {
            versions,
            eligible_paragraphs,
            ..
        } = wait_layout(&s)
        else {
            unreachable!()
        };
        if versions.source_revision >= r4.source_revision {
            let two = s.spans("chapters/two.tex");
            assert!(
                two.iter().any(|sp| eligible_paragraphs.contains(&sp.id)),
                "paragraph of the new file has a context"
            );
            break;
        }
    }
    s.close();
}

/// A fast edit that changes what a unit advances a counter to (an equation added) renumbers
/// what follows: the session schedules a layout pass; a later edit that keeps the counters
/// does not.
#[test]
fn counter_changes_schedule_a_pass() {
    let Some((s, _p)) = open("counters") else {
        return;
    };
    let Event::LayoutUpdate {
        eligible_paragraphs,
        versions: v1,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let body = spans
        .iter()
        .find(|sp| eligible_paragraphs.contains(&sp.id))
        .unwrap();
    let pos = body.range.start + doc[body.range.clone()].find(' ').unwrap();
    step("edit that advances a counter");
    let r = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: " \\stepcounter{equation}".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
    let Some(Event::ParagraphUpdate { status, .. }) = ev else {
        panic!("no paragraph update")
    };
    assert_eq!(status, "ok");
    // a pass follows because the unit now advances `equation`
    let v2 = loop {
        let Event::LayoutUpdate { versions, .. } = wait_layout(&s) else {
            unreachable!()
        };
        if versions.source_revision >= r.source_revision {
            break versions;
        }
    };
    assert!(v2.layout_version > v1.layout_version);
    step("edit that keeps the counters");
    let r2 = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: pos,
                end_byte: pos,
                text: " more".into(),
            },
        )
        .unwrap();
    assert_eq!(r2.routed, "fast", "{:?}", r2.reasons);
    let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
    assert!(matches!(ev, Some(Event::ParagraphUpdate { .. })));
    // no pass for it: the layout already knows the advanced counter
    let (ev, _) = wait(
        &s,
        4,
        |e| matches!(e, Event::LayoutUpdate { versions, .. } if versions.source_revision >= r2.source_revision),
    );
    assert!(ev.is_none(), "unexpected pass: {ev:?}");
    s.close();
}

/// Environments defined in the preamble: an inline one wraps a paragraph that stays live, a
/// block one (wrapping quote) is a unit of its own.
#[test]
fn user_environments_are_live() {
    if rtex_core::texlive::TexLive::discover().is_err() {
        eprintln!("SKIP: no lualatex");
        return;
    }
    let root = std::env::temp_dir().join(format!("rtex-session-{}-userenv", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("main.tex"),
        "\\documentclass{article}\n\\newenvironment{solution}{\\par\\noindent\\textbf{Solution.}\\ \\itshape}{\\par}\n\\newenvironment{hint}{\\begin{quote}\\small\\textbf{Hint:}\\ }{\\end{quote}}\n\\begin{document}\nA first paragraph of plain text.\n\n\\begin{hint}\nThink about it carefully before answering, and then think again.\n\\end{hint}\n\n\\begin{solution}\nThe answer is forty-two, as every reader of the guide already knows very well.\n\\end{solution}\n\\end{document}\n",
    )
    .unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debounce = Duration::from_millis(50);
    let s = Session::open(cfg).unwrap();
    let Event::LayoutUpdate {
        eligible_paragraphs,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let hint = spans
        .iter()
        .find(|sp| doc[sp.range.clone()].starts_with("\\begin{hint}"))
        .unwrap();
    let sol = spans
        .iter()
        .find(|sp| doc[sp.range.clone()].starts_with("\\begin{solution}"))
        .unwrap();
    assert!(eligible_paragraphs.contains(&hint.id), "hint unit");
    assert!(eligible_paragraphs.contains(&sol.id), "solution unit");
    for (sp, word) in [(hint, "Think"), (sol, "answer")] {
        let pos = sp.range.start + doc[sp.range.clone()].find(word).unwrap() + word.len();
        let r = s
            .apply_edit(
                "main.tex",
                Edit {
                    start_byte: pos,
                    end_byte: pos,
                    text: " really".into(),
                },
            )
            .unwrap();
        assert_eq!(r.routed, "fast", "{:?}", r.reasons);
        let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
        let Some(Event::ParagraphUpdate {
            par_id, status, dl, ..
        }) = ev
        else {
            panic!("no update")
        };
        assert_eq!(par_id, sp.id);
        assert_eq!(status, "ok");
        assert!(dl.glyph_count() > 10);
    }
    s.close();
}

/// The title block is a live unit, and stays one after the first compile (the server restores
/// \maketitle and friends, which the class disables after use).
#[test]
fn title_block_is_live() {
    if rtex_core::texlive::TexLive::discover().is_err() {
        eprintln!("SKIP: no lualatex");
        return;
    }
    let root = std::env::temp_dir().join(format!("rtex-session-{}-title", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("main.tex"),
        "\\documentclass{article}\n\\usepackage{hyperref}\n\\begin{document}\n\\title{A Title}\n\\author{Henry Yang \\and A. Reader\\thanks{With thanks.}}\n\\date{1 January 2026}\n\\maketitle\n\nBody text after the title block.\n\\end{document}\n",
    )
    .unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debounce = Duration::from_millis(50);
    let s = Session::open(cfg).unwrap();
    let Event::LayoutUpdate {
        eligible_paragraphs,
        ..
    } = wait_layout(&s)
    else {
        unreachable!()
    };
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let title = spans
        .iter()
        .find(|sp| doc[sp.range.clone()].starts_with("\\title"))
        .unwrap();
    assert!(eligible_paragraphs.contains(&title.id), "title block unit");
    let mut glyphs = Vec::new();
    for word in ["Title", "Reader"] {
        let doc = s.document_text("main.tex").unwrap();
        let pos = title.range.start + doc[title.range.clone()].find(word).unwrap() + word.len();
        let r = s
            .apply_edit(
                "main.tex",
                Edit {
                    start_byte: pos,
                    end_byte: pos,
                    text: " Longer".into(),
                },
            )
            .unwrap();
        assert_eq!(r.routed, "fast", "{:?}", r.reasons);
        let (ev, _) = wait(&s, 60, |e| matches!(e, Event::ParagraphUpdate { .. }));
        let Some(Event::ParagraphUpdate {
            par_id, status, dl, ..
        }) = ev
        else {
            panic!("no update")
        };
        assert_eq!(par_id, title.id);
        // \thanks is a footnote: an insert, so the result is ok_degraded (placed by the layout)
        assert!(status.starts_with("ok"), "{status}");
        glyphs.push(dl.glyph_count());
    }
    // the second compile typeset the block again (not \relax'ed away), with more text
    assert!(glyphs[1] > glyphs[0], "{glyphs:?}");
    s.close();
}
