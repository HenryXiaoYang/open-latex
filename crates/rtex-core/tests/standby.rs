//! The standby background engine (preamble preloaded, body streamed in) must produce the same
//! capture as a fresh run: same units, lines, rows and placements. Skipped without lualatex.

use rtex_core::background::{WarmEngine, read_aux_labels};
use rtex_core::capture::run_capture_with;
use rtex_core::fixtures::{generate, FontSet, Variant};
use rtex_core::texlive::TexLive;
use std::collections::BTreeMap;

#[test]
fn standby_pass_matches_fresh_pass() {
    let Ok(tl) = TexLive::discover() else {
        eprintln!("SKIP: no lualatex");
        return;
    };
    let root = std::env::temp_dir().join(format!("rtex-standby-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    generate(3, Variant::Units, FontSet::LmTfm, 1, &project).unwrap();
    let main = std::fs::read_to_string(project.join("main.tex")).unwrap();
    let mut files = BTreeMap::new();
    files.insert("main.tex".to_string(), main);
    // fresh run (two passes so labels exist on both sides)
    let fresh_out = root.join("fresh");
    run_capture_with(&tl, &project, "main.tex", &fresh_out, true, "").unwrap();
    let fresh = run_capture_with(&tl, &project, "main.tex", &fresh_out, true, "").unwrap();
    // standby: spawn, wait a moment (preamble loading), release
    let warm_out = root.join("warm");
    let body_dir = root.join("src-body");
    let w = WarmEngine::spawn(&tl, &project, &files, "main.tex", &body_dir, &warm_out, true, "").unwrap();
    let first = w.run(&project, &files, "main.tex").unwrap();
    assert!(first.exit_ok, "standby pass failed");
    let w2 = WarmEngine::spawn(&tl, &project, &files, "main.tex", &body_dir, &warm_out, true, "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let t0 = std::time::Instant::now();
    let warm = w2.run(&project, &files, "main.tex").unwrap();
    let warm_wall = t0.elapsed();
    assert!(warm.exit_ok);
    eprintln!("fresh pass {:.2}s, standby body pass {:.2}s", fresh.wall.as_secs_f64(), warm_wall.as_secs_f64());
    assert_eq!(warm.json.pages, fresh.json.pages);
    assert_eq!(warm.json.units.len(), fresh.json.units.len());
    for (a, b) in warm.json.units.iter().zip(fresh.json.units.iter()) {
        // files are compared by name: the TOC unit comes from <out_dir>/main.toc on each side
        let base = |f: &Option<String>| f.as_ref().map(|f| std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        assert_eq!((a.kind.clone(), a.name.clone(), base(&a.file), a.begin_line, a.end_line), (b.kind.clone(), b.name.clone(), base(&b.file), b.begin_line, b.end_line));
        assert_eq!(a.placements.len(), b.placements.len(), "unit {} rows", a.uid);
        for (pa, pb) in a.placements.iter().zip(b.placements.iter()) {
            assert_eq!((pa.page, pa.x, pa.y), (pb.page, pb.x, pb.y));
        }
    }
    assert_eq!(read_aux_labels(&warm_out.join("main.aux")), read_aux_labels(&fresh_out.join("main.aux")));
    for n in 1..=fresh.json.pages {
        assert_eq!(warm.page(n).unwrap().glyphs, fresh.page(n).unwrap().glyphs, "page {n} glyphs");
    }
}
