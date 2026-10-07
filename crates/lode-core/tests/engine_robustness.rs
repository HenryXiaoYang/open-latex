//! Robustness of the persistent paragraph server. Skipped (with a notice) when lualatex is
//! not available; run `source build/texlive.env` first.

use lode_core::capture::run_capture;
use lode_core::engine::{FastServer, Response};
use lode_core::fixtures::{generate, FontSet, Variant};
use lode_core::texlive::TexLive;
use std::time::Duration;

fn setup(name: &str) -> Option<(TexLive, std::path::PathBuf, std::path::PathBuf)> {
    let tl = match TexLive::discover() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SKIP: {e}");
            return None;
        }
    };
    let root = std::env::temp_dir().join(format!("lode-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    generate(2, Variant::Pure, FontSet::Pagella, 7, &project).unwrap();
    Some((tl, root, project))
}

fn preamble(project: &std::path::Path) -> String {
    let main = std::fs::read_to_string(project.join("main.tex")).unwrap();
    lode_core::split_preamble(&main).unwrap().0.to_string()
}

#[test]
fn errors_do_not_poison_the_server() {
    let Some((tl, root, project)) = setup("errors") else { return };
    let cap = run_capture(&tl, &project, "main.tex", &root.join("cap"), true).unwrap();
    let para = cap.json.paragraphs.iter().find(|p| p.is_top_level() && p.begin.is_some()).unwrap();
    let mut s = FastServer::spawn(&tl, &project, &root.join("serve"), &preamble(&project), 1).unwrap();
    s.set_context(para.seq, &para.context_json()).unwrap();
    let (ok, _) = s.compile(para.seq, "A plain paragraph of text that is fine.").unwrap();
    assert_eq!(ok.status, "ok");
    let lines_ok = ok.dl.unwrap().lines.len();

    // undefined macro → error status with a diagnostic, server keeps going
    let (bad, _) = s.compile(para.seq, "Text with \\nosuchmacro here.").unwrap();
    assert_eq!(bad.status, "error");
    assert!(bad.errors.iter().any(|e| e.message.as_deref().unwrap_or("").contains("Undefined control sequence")));

    // unbalanced brace → TeX inserts the missing brace; still an error, state stays consistent
    let (unb, _) = s.compile(para.seq, "Unbalanced { brace here.").unwrap();
    assert_eq!(unb.status, "error");

    // footnote → insert node: degraded (background path), but no error
    let (fn_, _) = s.compile(para.seq, "Footnote\\footnote{x} here.").unwrap();
    assert!(fn_.status == "ok_degraded" || fn_.status == "ok", "status {}", fn_.status);
    if fn_.status == "ok_degraded" {
        assert!(fn_.dl.unwrap().flags_map().contains_key("ins"));
    }

    // and the good paragraph still compiles identically
    let (again, _) = s.compile(para.seq, "A plain paragraph of text that is fine.").unwrap();
    assert_eq!(again.status, "ok");
    assert_eq!(again.dl.unwrap().lines.len(), lines_ok);
    match s.stats().unwrap() {
        Response::Stats { grouplevel, nest, .. } => {
            assert_eq!(grouplevel, 0);
            assert_eq!(nest, 0);
        }
        other => panic!("{other:?}"),
    }
    s.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn watchdog_kills_a_runaway_paragraph() {
    let Some((tl, root, project)) = setup("watchdog") else { return };
    let cap = run_capture(&tl, &project, "main.tex", &root.join("cap"), true).unwrap();
    let para = cap.json.paragraphs.iter().find(|p| p.is_top_level() && p.begin.is_some()).unwrap();
    let mut s = FastServer::spawn(&tl, &project, &root.join("serve"), &preamble(&project), 1).unwrap();
    s.set_context(para.seq, &para.context_json()).unwrap();
    s.timeout = Duration::from_millis(800);
    let t0 = std::time::Instant::now();
    let r = s.compile(para.seq, "\\loop\\iftrue\\repeat never ends");
    assert!(r.is_err(), "runaway paragraph must time out");
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(!s.is_alive(), "server must be killed by the watchdog");
    // a fresh generation works again
    let mut s2 = FastServer::spawn(&tl, &project, &root.join("serve"), &preamble(&project), 2).unwrap();
    s2.set_context(para.seq, &para.context_json()).unwrap();
    let (ok, _) = s2.compile(para.seq, "Back to normal.").unwrap();
    assert_eq!(ok.status, "ok");
    s2.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn paragraph_result_is_independent_of_request_order() {
    let Some((tl, root, project)) = setup("order") else { return };
    let cap = run_capture(&tl, &project, "main.tex", &root.join("cap"), true).unwrap();
    let paras: Vec<_> = cap.json.paragraphs.iter().filter(|p| p.is_top_level() && p.begin.is_some()).take(3).collect();
    let mut s = FastServer::spawn(&tl, &project, &root.join("serve"), &preamble(&project), 1).unwrap();
    for p in &paras {
        s.set_context(p.seq, &p.context_json()).unwrap();
    }
    let src = "Order independence: {\\itshape italic} and $x_1 + y^2$ with \\textbf{bold} words in it.";
    let (a, _) = s.compile(paras[0].seq, src).unwrap();
    let (_b, _) = s.compile(paras[1].seq, "Something else entirely, \\emph{different}.").unwrap();
    let (c, _) = s.compile(paras[0].seq, src).unwrap();
    assert_eq!(serde_json::to_string(&a.dl).unwrap(), serde_json::to_string(&c.dl).unwrap());
    s.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
