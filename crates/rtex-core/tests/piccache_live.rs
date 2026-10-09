//! The picture cache in the live engine: editing a sentence that shares its unit with a
//! plot re-typesets the text and places the cached plot (no drawing), and a block
//! environment the layout does not know yet (split off by a blank line) borrows a context
//! and is delivered live with its cached plot. Skipped without lualatex.

use rtex_core::session::EditResult;
use rtex_core::texlive::TexLive;
use rtex_core::{Edit, Event, Session, SessionConfig};
use rtex_dl::{DisplayList, Item};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A session on a copy of the pictures fixture with a converged layout (so the second
/// pass filled the cache). None without lualatex.
fn open(name: &str) -> Option<(PathBuf, Session)> {
    if let Err(e) = TexLive::discover() {
        eprintln!("SKIP: {e}");
        return None;
    }
    let src =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus/pictures");
    let root = std::env::temp_dir().join(format!("rtex-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::copy(src.join("main.tex"), project.join("main.tex")).unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.fast_budget = Duration::from_millis(5000); // timing is not what these tests measure
    cfg.debug_dir = Some(root.join("dbg"));
    let s = Session::open(cfg).unwrap();
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
    Some((root, s))
}

fn has_cached_picture(dl: &DisplayList) -> bool {
    dl.lines
        .iter()
        .flat_map(|l| l.items.iter())
        .any(|i| matches!(i, Item::Unsupported { kind, .. } if kind == "cached_picture"))
}

fn insert(s: &Session, root: &std::path::Path, needle: &str, text: &str) -> EditResult {
    let src = std::fs::read_to_string(root.join("project/main.tex")).unwrap();
    let off = src.find(needle).unwrap() + needle.len();
    s.apply_edit(
        "main.tex",
        Edit {
            start_byte: off,
            end_byte: off,
            text: text.into(),
        },
    )
    .unwrap()
}

#[test]
fn edited_sentence_keeps_its_plot_from_the_cache() {
    let Some((root, s)) = open("piccache-live") else {
        return;
    };
    let r = insert(&s, &root, "A sentence right before a plot,", " edited");
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
    assert!(
        has_cached_picture(&dl),
        "the plot should come from the cache; items: {:?}",
        dl.lines.iter().map(|l| l.items.len()).collect::<Vec<_>>()
    );
    // the compile costs the sentence, not the plot
    assert!(timing.tex_us < 100_000, "tex {} us", timing.tex_us);
    // and no probe ran: the probe compiles the snapshot's text, which would draw the plot
    // (seconds with macro tracing on, enough to trip the engine watchdog)
    assert_eq!(
        s.probes(),
        0,
        "a unit whose only unknown is a cached picture is not probed"
    );
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}

/// Two pictures in one unit, the first one edited: the second is still placed from the cache
/// (entries are matched by line, so the edited picture, which has no entry, is drawn and the
/// cached one stays the second).
#[test]
fn an_edited_picture_before_a_cached_one() {
    let Some((root, s)) = open("piccache-two") else {
        return;
    };
    let r = insert(&s, &root, "[anchor=east] {$n=1", "0");
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = None;
    while Instant::now() < deadline && seen.is_none() {
        for ev in s.poll(Duration::from_millis(200)) {
            if let Event::ParagraphUpdate { status, dl, .. } = ev {
                seen = Some((status, dl));
                break;
            }
        }
    }
    let (status, dl) = seen.expect("a paragraph update");
    assert_ne!(status, "error");
    // one row holds both pictures: the cached one is the right-hand one (after \hfill)
    let row = dl
        .lines
        .iter()
        .find(|l| {
            l.items
                .iter()
                .any(|i| matches!(i, Item::Unsupported { kind, .. } if kind == "cached_picture"))
        })
        .expect("a row with the cached picture");
    let cached: Vec<i64> = row
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Unsupported { kind, detail } if kind == "cached_picture" => detail
                .as_str()
                .and_then(|d| d.split(' ').nth(1))
                .and_then(|x| x.parse().ok()),
            _ => None,
        })
        .collect();
    assert_eq!(
        cached.len(),
        1,
        "exactly one picture from the cache: {cached:?}"
    );
    assert!(
        cached[0] > row.w / 2,
        "the cached picture is the second one (x {} of width {})",
        cached[0],
        row.w
    );
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}

/// A blank line typed between the sentence and its plot makes `\begin{center}` a unit of
/// its own that no layout pass has seen. It borrows the neighbouring paragraph's context
/// and is delivered live (approximate placement) with the plot from the cache.
#[test]
fn a_new_block_environment_borrows_a_context() {
    let Some((root, s)) = open("piccache-split") else {
        return;
    };
    let r = insert(&s, &root, "so the two are one unit:\n", "\n");
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    // the split replaces the unit by two fresh spans: the sentence and the environment
    let added = r.outcome.added.clone();
    assert_eq!(added.len(), 2, "two new spans: {:?}", r.outcome);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = std::collections::HashMap::new();
    let mut demoted = Vec::new();
    while Instant::now() < deadline && seen.len() < added.len() {
        for ev in s.poll(Duration::from_millis(200)) {
            match ev {
                Event::ParagraphUpdate {
                    par_id,
                    status,
                    dl,
                    fragments,
                    context_stale,
                    ..
                } if added.contains(&par_id) => {
                    seen.insert(par_id, (status, dl, fragments, context_stale));
                }
                Event::BackgroundScheduled {
                    par_id, reasons, ..
                } => demoted.push((par_id, reasons)),
                _ => {}
            }
        }
    }
    assert_eq!(
        seen.len(),
        2,
        "an update for both halves: {:?}; demoted: {demoted:?}; requests: {}",
        seen.keys(),
        std::fs::read_to_string(root.join("dbg/requests.log")).unwrap_or_default()
    );
    for (status, ..) in seen.values() {
        assert_ne!(status, "error");
    }
    // the environment half: the one with the plot, placed from the cache
    let (_, _, fragments, context_stale) = seen
        .values()
        .find(|(_, dl, ..)| has_cached_picture(dl))
        .expect("the environment's update carries the cached plot");
    assert!(context_stale, "a borrowed context is reported stale");
    assert!(
        fragments.iter().all(|f| f.approximate),
        "placement of a unit the layout never saw is approximate: {fragments:?}"
    );
    // placed after the sentence it was split from, not on top of it
    let (_, _, sentence, _) = seen
        .values()
        .find(|(_, dl, ..)| !has_cached_picture(dl))
        .expect("the sentence's update");
    let (sp, sl) = (sentence[0].page, *sentence[0].baselines.last().unwrap());
    let (ep, ef) = (fragments[0].page, fragments[0].baselines[0]);
    assert!(
        (ep, ef) > (sp, sl),
        "environment at page {ep} baseline {ef}, sentence ends at page {sp} baseline {sl}"
    );
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}
