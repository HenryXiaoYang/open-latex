//! An internal error in the server (a Lua error of ours while finishing a compile) is answered
//! at once: the unit falls back to the full compile without the engine being killed, nothing
//! loops, and the engine keeps serving other units. Before, the server stayed silent, the
//! watchdog killed the engine after 5 s and the unit was quarantined for the session.
//! Its own test binary: the environment variable reaches every server this process starts.
//! Skipped without lualatex.

use rtex_core::{Edit, Event, Session, SessionConfig};
use std::time::{Duration, Instant};

const MARKER: &str = "ZZINTERNALZZ";

#[test]
fn an_internal_error_falls_back_at_once_without_killing_the_engine() {
    if let Err(e) = rtex_core::texlive::TexLive::discover() {
        eprintln!("SKIP: {e}");
        return;
    }
    // SAFETY: set before any server is spawned; this binary holds only this test
    unsafe { std::env::set_var("RTEX_TEST_FINISH_ERROR", MARKER) };
    let root = std::env::temp_dir().join(format!("rtex-internal-error-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nA first paragraph that warms the engine up.\n\nA second paragraph that will carry the marker.\n\nA third paragraph that stays live.\n\\end{document}\n",
    )
    .unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("build");
    cfg.debug_dir = Some(root.join("debug"));
    let s = Session::open(cfg).unwrap();
    let wait_layout = |s: &Session| {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            assert!(Instant::now() < deadline, "no layout");
            for ev in s.poll(Duration::from_millis(100)) {
                if let Event::LayoutUpdate { convergence, .. } = ev {
                    if matches!(convergence, rtex_core::Convergence::Converged) {
                        return;
                    }
                }
            }
        }
    };
    wait_layout(&s);
    let edit = |needle: &str, text: &str| {
        let src = s.document_text("main.tex").unwrap();
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
    };
    // the marker makes this compile fail inside the server's finish
    let t0 = Instant::now();
    let r = edit("A second paragraph", &format!(" {MARKER}"));
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let mut fell_back = None;
    let mut restarted = false;
    let mut layouts = 0;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        for ev in s.poll(Duration::from_millis(100)) {
            match ev {
                Event::BackgroundScheduled { reasons, .. }
                    if reasons.iter().any(|r| r.contains("internal error")) =>
                {
                    fell_back.get_or_insert(t0.elapsed());
                }
                Event::EngineState { state, .. } if state == "Restarting" => restarted = true,
                Event::LayoutUpdate { .. } => layouts += 1,
                _ => {}
            }
        }
    }
    let after = fell_back.expect("an internal-error fallback");
    assert!(
        after < Duration::from_secs(2),
        "the fallback comes at once, not after the 5 s watchdog: {after:?}"
    );
    assert!(!restarted, "the engine is not killed for an internal error");
    assert!(
        layouts <= 3,
        "one pass for the fallback, no loop of passes: {layouts} layouts"
    );
    // the error is recorded with its traceback, once
    let bundles: Vec<_> = std::fs::read_dir(root.join("debug"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("engine-"))
        .collect();
    assert_eq!(bundles.len(), 1, "one bundle for the unit and layout");
    let report = std::fs::read_to_string(bundles[0].path().join("report.json")).unwrap();
    assert!(report.contains("injected error in finish"), "{report}");
    // the engine still serves other units live
    let r = edit("A third paragraph", " edited");
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut live = false;
    while Instant::now() < deadline && !live {
        for ev in s.poll(Duration::from_millis(100)) {
            if let Event::ParagraphUpdate { status, .. } = ev {
                if status == "ok" {
                    live = true;
                }
            }
        }
    }
    assert!(live, "another unit is still compiled live");
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}
