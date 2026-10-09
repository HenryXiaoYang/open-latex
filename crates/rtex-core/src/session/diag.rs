//! Diagnostics: debug bundles for engine failures, the live request log, TeX log parsing.

use super::*;

/// With `SessionConfig::debug_dir`: write everything needed to reproduce an engine failure
/// into `<debug_dir>/engine-<time>-g<generation>-par<id>/` and return that directory.
pub(super) fn debug_bundle(
    s: &Shared,
    req: &FastRequest,
    reason: &str,
    gen: u64,
    srv: Option<&FastServer>,
) -> Option<PathBuf> {
    let base = s.cfg.debug_dir.as_ref()?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = base.join(format!("engine-{ts}-g{gen}-par{}", req.par_id.0));
    let write = |name: &str, bytes: &[u8]| {
        let _ = std::fs::write(dir.join(name), bytes);
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    write("source.tex", req.source.as_bytes());
    if !req.pics.is_empty() {
        write("pics.json", req.pics.as_bytes());
    }
    let ctx = req.ctx.clone().or_else(|| {
        let layout = s.layout.lock();
        layout
            .units
            .iter()
            .find(|u| u.uid == req.seq)
            .map(|u| u.context_json())
    });
    if let Some(c) = &ctx {
        write(
            "context.json",
            serde_json::to_string_pretty(c)
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    let report = serde_json::json!({
        "reason": reason,
        "par_id": req.par_id.0,
        "edit_id": req.edit_id,
        "context_id": req.seq,
        "probe": req.probe,
        "warmup": req.warmup,
        "pictures_in_source": req.pics_n,
        "versions": req.versions,
        "engine_generation": gen,
        "compile_timeout_ms": s.cfg.compile_timeout.as_millis() as u64,
        "server": srv.map(|x| serde_json::json!({
            "banner": x.banner, "pid": x.pid(), "generation": x.generation,
            "startup_ms": x.startup.as_millis() as u64, "work_dir": x.work_dir,
        })),
        "rtex_version": env!("CARGO_PKG_VERSION"),
    });
    write(
        "report.json",
        serde_json::to_string_pretty(&report)
            .unwrap_or_default()
            .as_bytes(),
    );
    if let Some(x) = srv {
        for f in x.debug_files() {
            if f.is_file() {
                if let Some(name) = f.file_name() {
                    let _ = std::fs::copy(&f, dir.join(name));
                }
            }
        }
    }
    Some(dir)
}

/// With `SessionConfig::debug_dir`: a bundle for a server that failed to start (its driver,
/// preamble copy, TeX log and trace from `build/serve`), named in the returned message.
pub(super) fn debug_bundle_startup(s: &Shared, gen: u64, reason: &str) -> String {
    let Some(base) = s.cfg.debug_dir.as_ref() else {
        return reason.to_string();
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = base.join(format!("engine-{ts}-g{gen}-start"));
    if std::fs::create_dir_all(&dir).is_err() {
        return reason.to_string();
    }
    let serve = s.cfg.build_dir.join("serve");
    for name in [
        format!("rtex-serve-g{gen}.tex"),
        "rtex-preamble.tex".to_string(),
        format!("rtex-serve-g{gen}.log"),
        format!("rtex-serve-g{gen}.trace"),
    ] {
        let f = serve.join(&name);
        if f.is_file() {
            let _ = std::fs::copy(&f, dir.join(&name));
        }
    }
    let report = serde_json::json!({
        "reason": reason, "engine_generation": gen, "stage": "startup",
        "rtex_version": env!("CARGO_PKG_VERSION"),
    });
    let _ = std::fs::write(
        dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    );
    format!("{reason} (debug bundle: {})", dir.display())
}

/// With `SessionConfig::debug_dir`: one line per live compile in `<debug_dir>/requests.log`.
pub(super) fn debug_request_line(s: &Shared, line: &str) {
    let Some(base) = s.cfg.debug_dir.as_ref() else {
        return;
    };
    use std::io::Write;
    let _ = std::fs::create_dir_all(base);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.join("requests.log"))
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let _ = writeln!(f, "{ts:.3} {line}");
    }
}

/// Parse a LuaLaTeX log (with -file-line-error) into diagnostics.
pub fn parse_log(log: &Path) -> Vec<Diagnostic> {
    let Ok(text) = std::fs::read_to_string(log) else {
        return vec![];
    };
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let re_fle = regex::Regex::new(r"^(?P<file>[^:\s][^:]*):(?P<line>\d+): (?P<msg>.*)$").unwrap();
    let re_warn =
        regex::Regex::new(r"^(?:LaTeX|Package \w+|Class \w+) Warning: (?P<msg>.*)$").unwrap();
    let re_box =
        regex::Regex::new(r"^(Overfull|Underfull) \\[hv]box .* at lines (\d+)--(\d+)").unwrap();
    for (i, l) in lines.iter().enumerate() {
        if let Some(c) = re_fle.captures(l) {
            if c["msg"].starts_with("Undefined") || !c["msg"].is_empty() {
                out.push(Diagnostic {
                    severity: "error".into(),
                    file: Some(c["file"].to_string()),
                    line: c["line"].parse().ok(),
                    message: c["msg"].to_string(),
                    context: lines.get(i + 1).map(|s| s.to_string()),
                });
            }
        } else if let Some(c) = re_warn.captures(l) {
            out.push(Diagnostic {
                severity: "warning".into(),
                file: None,
                line: None,
                message: c["msg"].to_string(),
                context: None,
            });
        } else if let Some(c) = re_box.captures(l) {
            out.push(Diagnostic {
                severity: "info".into(),
                file: None,
                line: c[2].parse().ok(),
                message: l.to_string(),
                context: None,
            });
        } else if l.starts_with("! ") {
            out.push(Diagnostic {
                severity: "error".into(),
                file: None,
                line: None,
                message: l[2..].to_string(),
                context: lines.get(i + 1).map(|s| s.to_string()),
            });
        }
    }
    out
}
