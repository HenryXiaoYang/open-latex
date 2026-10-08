//! The extended fast path through a session: every unit kind of the `units` fixture is routed
//! fast, compiles without error, comes back with fragments, and respects the budget. Skipped
//! without lualatex.

use rtex_core::fixtures::{generate, FontSet, Variant};
use rtex_core::texlive::TexLive;
use rtex_core::{Edit, Event, Session, SessionConfig};
use std::time::Duration;

fn setup(name: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    if let Err(e) = TexLive::discover() {
        eprintln!("SKIP: {e}");
        return None;
    }
    let root = std::env::temp_dir().join(format!("rtex-units-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    generate(10, Variant::Units, FontSet::LmTfm, 3, &project).unwrap();
    Some((root, project))
}

fn kind_of(text: &str) -> &'static str {
    let t = text.trim_start();
    if t.starts_with("\\chapter") || t.starts_with("\\section") {
        "heading"
    } else if t.starts_with("\\begin{figure}") {
        "figure"
    } else if t.starts_with("\\begin{table}") {
        "table"
    } else if t.starts_with("\\begin{itemize}") {
        "list"
    } else if t.starts_with("\\begin{theorem}") {
        "theorem"
    } else if t.starts_with("\\begin{quote}") {
        "quote"
    } else if text.contains("\\begin{align}") {
        "align"
    } else if text.contains("\\begin{equation}") {
        "equation"
    } else if text.contains("\\[") {
        "display"
    } else if text.contains("\\footnote") {
        "footnote"
    } else if text.contains("\\ref{") {
        "refs"
    } else {
        "plain"
    }
}

/// Byte offset just after the first letter of body text: control words, the arguments of
/// `\begin`/`\end`/`\label`/`\ref`-like commands and `\includegraphics`, and option groups are
/// skipped; `\caption{…}` text is preferred when present.
fn edit_offset(text: &str) -> usize {
    if let Some(c) = text.find("\\caption{") {
        return c + "\\caption{".len() + edit_offset(&text[c + "\\caption{".len()..]);
    }
    let b = text.as_bytes();
    let skip_group = |mut i: usize, open: u8, close: u8| -> usize {
        if i < b.len() && b[i] == open {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == open { depth += 1 } else if b[i] == close { depth -= 1; if depth == 0 { return i + 1; } }
                i += 1;
            }
        }
        i
    };
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let start = i + 1;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'@') {
                i += 1;
            }
            let name = &text[start..i];
            let args = matches!(name, "begin" | "end" | "label" | "ref" | "eqref" | "pageref" | "cite" | "includegraphics");
            i = skip_group(i, b'[', b']');
            if args {
                i = skip_group(i, b'{', b'}');
                i = skip_group(i, b'[', b']');
                i = skip_group(i, b'{', b'}');
            }
            continue;
        }
        if b[i].is_ascii_lowercase() {
            return i + 1;
        }
        i += 1;
    }
    0
}

#[test]
fn every_unit_kind_takes_the_fast_path() {
    let Some((root, project)) = setup("kinds") else { return };
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.fast_budget = Duration::from_millis(50); // first compiles load fonts
    let s = Session::open(cfg).unwrap();
    let (first, _) = s.wait_for(Duration::from_secs(300), |e| matches!(e, Event::LayoutUpdate { .. }));
    let Some(Event::LayoutUpdate { eligible_paragraphs, placements, .. }) = first else { panic!("no layout") };
    s.pause_background(true);
    std::thread::sleep(Duration::from_millis(1500)); // warm-up compile
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let mut seen: std::collections::BTreeMap<&str, (u32, u32)> = std::collections::BTreeMap::new();
    for sp in &spans {
        if !eligible_paragraphs.contains(&sp.id) {
            continue;
        }
        let text = doc[sp.range.clone()].to_string();
        let kind = kind_of(&text);
        // a character edit after the first letter that is not part of a control word
        let pos = sp.range.start + edit_offset(&text);
        let r = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: "x".into() }).unwrap();
        assert_eq!(r.routed, "fast", "{kind} unit {:?} not routed fast: {:?}\n{text}", sp.id, r.reasons);
        let (ev, others) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
        let Some(Event::ParagraphUpdate { status, fragments, dl, diagnostics, timing, pagination_stale, .. }) = ev else {
            panic!("no paragraph update for {kind} unit {:?}; events {:?}", sp.id, others.iter().map(|e| serde_json::to_value(e).map(|v| v["event"].to_string()).unwrap_or_default()).collect::<Vec<_>>())
        };
        assert!(status == "ok" || status == "ok_degraded", "{kind}: status {status} {diagnostics:?}");
        assert!(!fragments.is_empty(), "{kind}: no fragments");
        assert!(!dl.lines.is_empty(), "{kind}: empty display list");
        let pl = placements.iter().find(|p| p.par_id == sp.id).unwrap();
        // one character may reflow a tight unit by one row; then the pagination is flagged stale
        let delta = dl.lines.len() as i64 - pl.lines;
        assert!(delta.abs() <= 1, "{kind}: row count {} vs layout {}", dl.lines.len(), pl.lines);
        if delta != 0 {
            assert!(pagination_stale, "{kind}: row count changed but pagination_stale is false");
        }
        let e = seen.entry(kind).or_insert((0, 0));
        e.0 += 1;
        e.1 = e.1.max(timing.total_us as u32);
        // undo
        s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos + 1, text: String::new() }).unwrap();
        let _ = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
    }
    eprintln!("fast-path units by kind (count, slowest µs): {seen:?}");
    for k in ["heading", "figure", "table", "list", "theorem", "quote", "align", "equation", "display", "footnote", "refs", "plain"] {
        assert!(seen.contains_key(k), "no eligible {k} unit in the fixture; saw {seen:?}");
    }
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn over_budget_units_fall_back_to_background() {
    let Some((root, project)) = setup("budget") else { return };
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.fast_budget = Duration::from_micros(1); // everything is over budget
    let s = Session::open(cfg).unwrap();
    let (first, _) = s.wait_for(Duration::from_secs(300), |e| matches!(e, Event::LayoutUpdate { .. }));
    let Some(Event::LayoutUpdate { eligible_paragraphs, .. }) = first else { panic!("no layout") };
    s.pause_background(true);
    std::thread::sleep(Duration::from_millis(1500));
    let doc = s.document_text("main.tex").unwrap();
    let spans = s.spans("main.tex");
    let sp = spans.iter().find(|sp| eligible_paragraphs.contains(&sp.id) && !doc[sp.range.clone()].trim_start().starts_with('\\'))
        .unwrap_or_else(|| panic!("no eligible plain unit; eligible texts: {:?}", spans.iter().filter(|sp| eligible_paragraphs.contains(&sp.id)).map(|sp| doc[sp.range.clone()].chars().take(30).collect::<String>()).collect::<Vec<_>>()));
    let pos = sp.range.start + edit_offset(&doc[sp.range.clone()]);
    // the first two over-budget compiles are forgiven (fonts may load); the third marks the unit
    for _ in 0..3 {
        let r = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: "x".into() }).unwrap();
        assert_eq!(r.routed, "fast", "{:?}", r.reasons);
        let (ev, _) = s.wait_for(Duration::from_secs(60), |e| matches!(e, Event::ParagraphUpdate { .. }));
        assert!(ev.is_some());
    }
    // the third result arrived, and the unit is now over budget: the next edit goes to the background
    let (bg, _) = s.wait_for(Duration::from_secs(10), |e| matches!(e, Event::BackgroundScheduled { reasons, .. } if reasons.iter().any(|r| r.contains("over the budget"))));
    assert!(bg.is_some(), "no over-budget notice");
    let r2 = s.apply_edit("main.tex", Edit { start_byte: pos, end_byte: pos, text: "y".into() }).unwrap();
    assert_eq!(r2.routed, "background", "{:?}", r2.reasons);
    assert!(r2.reasons.iter().any(|r| r.contains("over the budget")), "{:?}", r2.reasons);
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}
