//! The picture cache in the live engine: editing a sentence that shares its unit with a
//! plot re-typesets the text and places the cached plot (no drawing). Skipped without
//! lualatex.

use rtex_core::texlive::TexLive;
use rtex_core::{Edit, Event, Session, SessionConfig};
use rtex_dl::Item;
use std::time::{Duration, Instant};

#[test]
fn edited_sentence_keeps_its_plot_from_the_cache() {
    if let Err(e) = TexLive::discover() {
        eprintln!("SKIP: {e}");
        return;
    }
    let src =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus/pictures");
    let root = std::env::temp_dir().join(format!("rtex-piccache-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::copy(src.join("main.tex"), project.join("main.tex")).unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.fast_budget = Duration::from_millis(5000); // timing is not what this test measures
    let s = Session::open(cfg).unwrap();
    // a converged layout after at least two passes: the second pass filled the cache
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut converged = false;
    while !converged {
        for ev in s.poll(Duration::from_millis(200)) {
            if let Event::LayoutUpdate { convergence, .. } = ev {
                if matches!(convergence, rtex_core::Convergence::Converged) {
                    converged = true;
                }
            }
        }
        assert!(Instant::now() < deadline, "no converged layout");
    }
    let text = std::fs::read_to_string(project.join("main.tex")).unwrap();
    let needle = "A sentence right before a plot,";
    let off = text.find(needle).unwrap() + needle.len();
    let r = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: off,
                end_byte: off,
                text: " edited".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = None;
    while Instant::now() < deadline && seen.is_none() {
        for ev in s.poll(Duration::from_millis(200)) {
            if let Event::ParagraphUpdate {
                status, dl, timing, ..
            } = ev
            {
                seen = Some((status, dl, timing));
                break;
            }
        }
    }
    let (status, dl, timing) = seen.expect("a paragraph update");
    assert_ne!(status, "error");
    let cached = dl
        .lines
        .iter()
        .flat_map(|l| l.items.iter())
        .any(|i| matches!(i, Item::Unsupported { kind, .. } if kind == "cached_picture"));
    assert!(
        cached,
        "the plot should come from the cache; items: {:?}",
        dl.lines.iter().map(|l| l.items.len()).collect::<Vec<_>>()
    );
    // the compile costs the sentence, not the plot
    assert!(timing.tex_us < 100_000, "tex {} us", timing.tex_us);
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}
