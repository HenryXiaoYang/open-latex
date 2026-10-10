//! A session whose project and build directories have spaces and non-ASCII characters (a
//! Windows user profile such as `C:\Users\Lê Anh\AppData\…`, where editors keep their storage),
//! with a CRLF document. Skipped without lualatex.

use rtex_core::{Convergence, Edit, Event, Session, SessionConfig};
use std::time::Duration;

#[test]
fn live_editing_works_with_spaces_unicode_and_crlf() {
    if rtex_core::texlive::TexLive::discover().is_err() {
        eprintln!("SKIP: no lualatex");
        return;
    }
    let root = std::env::temp_dir().join(format!("rtex paths Lê Anh Dũng {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("my project");
    std::fs::create_dir_all(&project).unwrap();
    let main = "\\documentclass{article}\r\n\\begin{document}\r\nFirst document.\r\n\
                This is a simple example, with no extra parameters or packages included.\r\n\r\n\
                love love love\r\n\\end{document}\r\n";
    std::fs::write(project.join("main.tex"), main).unwrap();
    let mut cfg = SessionConfig::new(&project, "main.tex");
    cfg.build_dir = root.join("work storage").join("build");
    cfg.fast_budget = Duration::from_millis(5000); // timing is not what this test measures
    let s = Session::open(cfg).unwrap();

    // the live engine must start (a failed start is reported with the TeX log's first error)
    let (ev, others) = s.wait_for(Duration::from_secs(120), |e| {
        matches!(e, Event::EngineState { state, .. } if state == "Ready" || state == "Failed" || state == "Restarting")
    });
    match &ev {
        Some(Event::EngineState { state, .. }) if state == "Ready" => {}
        other => {
            // the whole server log, for platforms where the excerpt is not enough
            let serve = root.join("work storage").join("build").join("serve");
            for e in std::fs::read_dir(&serve).into_iter().flatten().flatten() {
                if e.path().extension().is_some_and(|x| x == "log") {
                    let t = std::fs::read_to_string(e.path()).unwrap_or_default();
                    eprintln!("==== {}\n{t}", e.path().display());
                }
            }
            panic!("live engine did not start: {other:?}")
        }
    }
    for o in others {
        s.requeue(o);
    }
    let (lay, _) = s.wait_for(Duration::from_secs(180), |e| {
        matches!(
            e,
            Event::LayoutUpdate {
                convergence: Convergence::Converged | Convergence::PassLimitReached { .. },
                ..
            }
        )
    });
    assert!(lay.is_some(), "no layout");

    let at = main.find("love love love").unwrap() + "love love love".len();
    let r = s
        .apply_edit(
            "main.tex",
            Edit {
                start_byte: at,
                end_byte: at,
                text: " love".into(),
            },
        )
        .unwrap();
    assert_eq!(r.routed, "fast", "{:?}", r.reasons);
    let (ev, others) = s.wait_for(Duration::from_secs(60), |e| {
        matches!(
            e,
            Event::ParagraphUpdate { .. } | Event::BackgroundScheduled { .. }
        )
    });
    match ev {
        Some(Event::ParagraphUpdate { status, .. }) => assert_eq!(status, "ok"),
        other => panic!("the edit was not live: {other:?}; other events: {others:?}"),
    }
    s.close();
    let _ = std::fs::remove_dir_all(&root);
}
