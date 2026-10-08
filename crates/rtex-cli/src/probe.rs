//! `rtex probe`: developer-facing latency breakdown of the fast path on one project.
//! Direct server round trips (no session threads), in-engine profile counters, and session
//! round trips (apply_edit → ParagraphUpdate) for the shortest and a medium paragraph.

use anyhow::{bail, Result};
use rtex_core::capture::run_capture;
use rtex_core::engine::{FastServer, Response};
use rtex_core::texlive::TexLive;
use rtex_core::{Edit, Event, Session, SessionConfig};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn med(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}
fn p95(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64) * 0.95) as usize]
}

pub fn run(project: PathBuf, main: String, n: usize, build: PathBuf) -> Result<()> {
    let tl = TexLive::discover()?;
    let project = project.canonicalize()?;
    std::fs::create_dir_all(&build)?;
    let cap = run_capture(&tl, &project, &main, &build.join("capture"), true)?;
    let main_text = std::fs::read_to_string(project.join(&main))?;
    let (preamble, _) = rtex_core::split_preamble(&main_text).unwrap();
    let policy = rtex_core::eligibility::Policy::from_preamble(preamble, &[], &[]);
    let (store, fb) = rtex_core::layout::LayoutStore::offline(&cap, &project, &main, &policy)?;
    let mut cands: Vec<(&rtex_core::layout::EngineUnit, rtex_core::ParaId)> = store.mapped_units().into_iter().filter(|(u, _)| u.kind() == "par" && u.rows() > 0 && u.first_para.as_ref().map(|p| p.begin.is_some()).unwrap_or(false)).collect();
    cands.sort_by_key(|(u, _)| u.rows());
    let short = cands.iter().copied().find(|(u, _)| u.rows() == 1);
    let medium = cands.iter().copied().find(|(u, _)| (4..=5).contains(&u.rows()));
    let long = cands.iter().copied().find(|(u, _)| u.rows() >= 10);
    let mut server = FastServer::spawn(&tl, &project, &build.join("serve"), preamble, 1)?;
    let mut pings = Vec::new();
    for _ in 0..200 {
        pings.push(server.ping()?.as_secs_f64() * 1e3);
    }
    println!("ping: median {:.3} ms, P95 {:.3} ms", med(&mut pings), p95(&mut pings));
    for (name, p) in [("short", short), ("medium", medium), ("long", long)] {
        let Some((p, span_id)) = p else { continue };
        let src = fb.span_text(span_id).unwrap_or("").trim_end_matches('\n').to_string();
        server.set_context(p.uid, &p.context_json())?;
        for _ in 0..5 {
            server.compile(p.uid, &src)?;
        }
        let (mut tot, mut tex, mut trav) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..n {
            let s = if i % 2 == 1 { format!("{src} x") } else { src.clone() };
            let (r, rt) = server.compile(p.uid, &s)?;
            if r.status != "ok" {
                bail!("{name}: status {}", r.status);
            }
            tot.push(rt.total.as_secs_f64() * 1e3);
            tex.push(rt.t_tex.as_secs_f64() * 1e3);
            trav.push(rt.t_traverse.as_secs_f64() * 1e3);
            std::thread::sleep(Duration::from_millis(2));
        }
        let (t, a, b) = (med(&mut tot), med(&mut tex), med(&mut trav));
        println!("direct {name:6} (unit {} rows {}): total {:.3} ms (P95 {:.3}) tex {:.3} traverse {:.3} ipc+parse {:.3}", p.uid, p.rows(), t, p95(&mut tot), a, b, t - a - b);
        let (r, _) = server.compile(p.uid, &src)?;
        println!("  engine stages(us): {}  host stages(us) send/wait/read/parse: {:?}", r.stages_us, r.host_us);
        if let Some(b) = &r.dl_binary {
            let path = build.join(format!("{name}.dl"));
            let same = std::fs::read(&path).map(|old| old == *b).ok();
            std::fs::write(&path, b)?;
            println!("  display list {} bytes written to {} (identical to previous run: {:?})", b.len(), path.display(), same);
        }
        if let Response::Profile { us, .. } = server.profile(p.uid, &src, 40)? {
            println!("  profile(us): {}", us);
        }
    }
    // one unit of every other kind (env:name / heading:name): same direct round trips
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (u, span_id) in store.mapped_units() {
        let c = &u.captured;
        if c.kind == "par" || c.placements.is_empty() {
            continue;
        }
        let key = format!("{}:{}", c.kind, c.name.clone().unwrap_or_default());
        if !seen.insert(key.clone()) {
            continue;
        }
        let src = fb.span_text(span_id).unwrap_or("").trim_end_matches('\n').to_string();
        let (_shape, reasons) = rtex_core::eligibility::classify_source(&src, &policy);
        if !reasons.is_empty() {
            continue;
        }
        server.set_context(u.uid, &u.context_json())?;
        for _ in 0..5 {
            server.compile(u.uid, &src)?;
        }
        // the alternate source differs in bytes but not in layout (a doubled inter-word space)
        let alt = src.replacen(' ', "  ", 1);
        let (mut tot, mut tex, mut trav) = (Vec::new(), Vec::new(), Vec::new());
        let mut last = None;
        for i in 0..n {
            let s = if i % 2 == 1 { &alt } else { &src };
            let (r, rt) = server.compile(u.uid, s)?;
            if r.status != "ok" {
                bail!("{key}: status {} {:?}", r.status, r.errors);
            }
            tot.push(rt.total.as_secs_f64() * 1e3);
            tex.push(rt.t_tex.as_secs_f64() * 1e3);
            trav.push(rt.t_traverse.as_secs_f64() * 1e3);
            last = Some(r);
            std::thread::sleep(Duration::from_millis(2));
        }
        let (t, a, b) = (med(&mut tot), med(&mut tex), med(&mut trav));
        println!("direct {key:16} (unit {} rows {} dl_bytes {}): total {:.3} ms (P95 {:.3}) tex {:.3} traverse {:.3} ipc+parse {:.3}", u.uid, u.rows(), last.as_ref().map(|r| r.dl_bytes).unwrap_or(0), t, p95(&mut tot), a, b, t - a - b);
        if let Some(r) = last {
            println!("  engine stages(us): {}", r.stages_us);
        }
    }
    server.shutdown()?;

    // session-level: the same edits through apply_edit/poll
    let mut cfg = SessionConfig::new(&project, main.clone());
    cfg.build_dir = build.join("session");
    let session = Session::open(cfg)?;
    let (first, _) = session.wait_for(Duration::from_secs(600), |e| matches!(e, Event::LayoutUpdate { .. }));
    let Some(Event::LayoutUpdate { eligible_paragraphs, placements, .. }) = first else { bail!("no layout") };
    session.pause_background(true);
    std::thread::sleep(Duration::from_millis(1500));
    let spans = session.spans(&main);
    let doc = session.document_text(&main).unwrap();
    for (name, want) in [("short", 1usize), ("medium", 4), ("long", 10)] {
        let Some(pl) = placements.iter().find(|p| eligible_paragraphs.contains(&p.par_id) && (p.lines as usize == want || (want == 10 && p.lines >= 10))) else { continue };
        let sp = spans.iter().find(|s| s.id == pl.par_id).unwrap();
        let pos = sp.range.start + doc[sp.range.clone()].find(' ').unwrap_or(0);
        let mut toggled = false;
        let (mut tot, mut eng, mut tex, mut trav, mut host_pre) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut last_host = [0u64; 3];
        for i in 0..n + 5 {
            let edit = if toggled { Edit { start_byte: pos, end_byte: pos + 2, text: String::new() } } else { Edit { start_byte: pos, end_byte: pos, text: " x".into() } };
            toggled = !toggled;
            let t0 = Instant::now();
            let r = session.apply_edit(&main, edit)?;
            let t_apply = t0.elapsed();
            if r.routed != "fast" {
                bail!("not fast: {:?}", r.reasons);
            }
            last_host = r.host_us;
            let (ev, _) = session.wait_for(Duration::from_secs(30), |e| matches!(e, Event::ParagraphUpdate { .. }));
            let total = t0.elapsed();
            let Some(Event::ParagraphUpdate { timing, .. }) = ev else { bail!("no update") };
            if i >= 5 {
                tot.push(total.as_secs_f64() * 1e3);
                eng.push(timing.total_us as f64 / 1e3);
                tex.push(timing.tex_us as f64 / 1e3);
                trav.push(timing.traverse_us as f64 / 1e3);
                host_pre.push(t_apply.as_secs_f64() * 1e3);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let (t, e, a, b, h) = (med(&mut tot), med(&mut eng), med(&mut tex), med(&mut trav), med(&mut host_pre));
        println!("session {name:6} (par {}): total {:.3} ms (P95 {:.3}); engine compile {:.3}; tex {:.3} traverse {:.3}; apply_edit {:.3}; thread hops+events {:.3}; ipc+parse {:.3}",
            pl.par_id.0, t, p95(&mut tot), e, a, b, h, t - e - h, e - a - b);
        println!("  apply_edit stages(us) segment/eligibility/dispatch: {:?}", last_host);
    }
    session.close();
    Ok(())
}
