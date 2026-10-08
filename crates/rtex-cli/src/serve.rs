//! `rtex serve`: JSON-lines front end to a Session for scripted hosts and tests.
//! Commands on stdin (one JSON object per line); events and replies on stdout.

use anyhow::Result;
use rtex_core::{Edit, Session, SessionConfig};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

pub fn run(
    project: PathBuf,
    main: String,
    build: Option<PathBuf>,
    fast_budget_ms: u64,
) -> Result<()> {
    let mut cfg = SessionConfig::new(project, main);
    cfg.fast_budget = std::time::Duration::from_millis(fast_budget_ms);
    if let Some(b) = build {
        cfg.build_dir = b;
    }
    let session = Session::open(cfg)?;
    let stdout = std::io::stdout();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    // event pump
    let session = std::sync::Arc::new(session);
    {
        let s = session.clone();
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            for e in s.poll(Duration::from_millis(100)) {
                if tx.send(serde_json::to_string(&e).unwrap()).is_err() {
                    return;
                }
            }
        });
    }
    // writer
    std::thread::spawn(move || {
        let mut out = stdout.lock();
        for line in rx {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    });
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tx.send(
                    serde_json::json!({"reply": "error", "message": format!("bad json: {e}")})
                        .to_string(),
                )
                .ok();
                continue;
            }
        };
        let cmd = v.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
        let reply = match cmd {
            "edit" => {
                let path = v
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("main.tex")
                    .to_string();
                let edit = Edit {
                    start_byte: v.get("start").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                    end_byte: v.get("end").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                    text: v
                        .get("text")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                };
                match session.apply_edit(&path, edit) {
                    Ok(r) => serde_json::json!({"reply": "edit", "result": r}),
                    Err(e) => serde_json::json!({"reply": "error", "message": e.to_string()}),
                }
            }
            "set_document" => {
                let path = v
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("main.tex")
                    .to_string();
                let text = v
                    .get("text")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                match session.set_document(&path, &text) {
                    Ok(r) => serde_json::json!({"reply": "set_document", "result": r}),
                    Err(e) => serde_json::json!({"reply": "error", "message": e.to_string()}),
                }
            }
            "spans" => {
                let path = v
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("main.tex")
                    .to_string();
                serde_json::json!({"reply": "spans", "spans": session.spans(&path)})
            }
            "status" => {
                serde_json::json!({"reply": "status", "versions": session.versions(), "convergence": session.convergence()})
            }
            "request_layout" => {
                session.request_layout();
                serde_json::json!({"reply": "request_layout"})
            }
            "export_pdf" => {
                let out = v
                    .get("out")
                    .and_then(|p| p.as_str())
                    .unwrap_or("out.pdf")
                    .to_string();
                serde_json::json!({"reply": "export_pdf", "job_id": session.export_pdf(out)})
            }
            "quit" => break,
            other => {
                serde_json::json!({"reply": "error", "message": format!("unknown cmd {other}")})
            }
        };
        tx.send(reply.to_string()).ok();
    }
    Ok(())
}

/// `rtex edit`: open a session, wait for the first layout, apply one edit, print the paragraph
/// update (or the background routing) and exit.
pub fn edit_once(
    project: PathBuf,
    main: String,
    byte: Option<usize>,
    find: Option<String>,
    text: String,
    wait_s: u64,
) -> Result<()> {
    let session = Session::open(SessionConfig::new(project, main.clone()))?;
    let (first, _) = session.wait_for(Duration::from_secs(wait_s), |e| {
        matches!(e, rtex_core::Event::LayoutUpdate { .. })
    });
    if let Some(rtex_core::Event::LayoutUpdate {
        versions,
        convergence,
        pages_total,
        eligible_paragraphs,
        wall_ms,
        ..
    }) = &first
    {
        println!(
            "layout v{} ({} pages, {} eligible paragraphs, {:?}) in {} ms",
            versions.layout_version,
            pages_total,
            eligible_paragraphs.len(),
            convergence,
            wall_ms
        );
    } else {
        anyhow::bail!("no layout within {wait_s}s");
    }
    let doc = session.document_text(&main).unwrap();
    let pos = match (byte, &find) {
        (Some(b), _) => b,
        (None, Some(f)) => {
            doc.find(f.as_str())
                .ok_or_else(|| anyhow::anyhow!("pattern not found"))?
                + f.len()
        }
        _ => anyhow::bail!("need --byte or --find"),
    };
    let t0 = std::time::Instant::now();
    let r = session.apply_edit(
        &main,
        Edit {
            start_byte: pos,
            end_byte: pos,
            text,
        },
    )?;
    println!(
        "edit {} rev {} routed {} {:?}",
        r.edit_id, r.source_revision, r.routed, r.reasons
    );
    if r.routed == "fast" {
        let (ev, _) = session.wait_for(Duration::from_secs(wait_s), |e| {
            matches!(e, rtex_core::Event::ParagraphUpdate { .. })
        });
        match ev {
            Some(rtex_core::Event::ParagraphUpdate {
                par_id,
                status,
                fragments,
                pagination_stale,
                context_stale,
                dl,
                timing,
                diagnostics,
                ..
            }) => {
                println!("paragraph {:?} {} in {:.3} ms (tex {:.3}, traverse {:.3}, pack {:.3}); {} lines, {} glyphs, {} fragments, pagination_stale={} context_stale={} host wall {:.3} ms",
                    par_id, status, timing.total_us as f64 / 1e3, timing.tex_us as f64 / 1e3, timing.traverse_us as f64 / 1e3, timing.pack_us as f64 / 1e3,
                    dl.lines.len(), dl.glyph_count(), fragments.len(), pagination_stale, context_stale, t0.elapsed().as_secs_f64() * 1e3);
                for f in &fragments {
                    println!(
                        "  fragment page {} lines {}..{} x={} baselines={:?}",
                        f.page,
                        f.first_line,
                        f.last_line,
                        f.x,
                        &f.baselines[..f.baselines.len().min(4)]
                    );
                }
                for d in diagnostics {
                    println!("  diag: {} line {:?}", d.message, d.line);
                }
            }
            _ => println!("no paragraph update within {wait_s}s"),
        }
    }
    session.close();
    Ok(())
}
