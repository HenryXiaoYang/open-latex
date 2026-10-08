//! C ABI. Opaque handles; strings are UTF-8 and owned by the library until freed with
//! `rtex_string_free`; display lists are binary (encoding revision 1) buffers owned by the event.
#![allow(clippy::missing_safety_doc)]

use crate::document::Edit;
use crate::session::{Event, Session, SessionConfig};
use rtex_dl::DisplayList;
use std::ffi::{c_char, CStr, CString};
use std::time::Duration;

pub struct RtexSession(Session);

pub struct RtexEvent {
    kind: u32,
    json: CString,
    dls: Vec<Vec<u8>>,
}

fn cstr(s: String) -> *mut c_char {
    CString::new(s.replace('\0', " ")).unwrap().into_raw()
}

unsafe fn rstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

#[no_mangle]
pub extern "C" fn rtex_version() -> *const c_char {
    static V: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    V.as_ptr() as *const c_char
}

/// Open a session. `config_json`: {"project_root": "...", "main_file": "main.tex",
/// "build_dir": "...", "debounce_ms": 300, "max_passes": 5, "trusted_macros": [...],
/// "fast_on_stale_context": true, "compile_timeout_ms": 5000, "fast_budget_ms": 5, "unit_envs": [...], "warm_background": true}. On failure returns null and
/// writes an error message to `*err_out` (free with rtex_string_free).
#[no_mangle]
pub unsafe extern "C" fn rtex_session_open(
    config_json: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut RtexSession {
    let fail = |msg: String| {
        if !err_out.is_null() {
            *err_out = cstr(msg);
        }
        std::ptr::null_mut()
    };
    let Some(cfg_s) = rstr(config_json) else {
        return fail("config_json is null or not UTF-8".into());
    };
    let v: serde_json::Value = match serde_json::from_str(cfg_s) {
        Ok(v) => v,
        Err(e) => return fail(format!("config: {e}")),
    };
    let Some(root) = v.get("project_root").and_then(|x| x.as_str()) else {
        return fail("config.project_root missing".into());
    };
    let main = v
        .get("main_file")
        .and_then(|x| x.as_str())
        .unwrap_or("main.tex");
    let mut cfg = SessionConfig::new(root, main);
    if let Some(b) = v.get("build_dir").and_then(|x| x.as_str()) {
        cfg.build_dir = b.into();
    }
    if let Some(ms) = v.get("debounce_ms").and_then(|x| x.as_u64()) {
        cfg.debounce = Duration::from_millis(ms);
    }
    if let Some(n) = v.get("max_passes").and_then(|x| x.as_u64()) {
        cfg.max_passes = n as u32;
    }
    if let Some(ms) = v.get("compile_timeout_ms").and_then(|x| x.as_u64()) {
        cfg.compile_timeout = Duration::from_millis(ms);
    }
    if let Some(ms) = v.get("fast_budget_ms").and_then(|x| x.as_u64()) {
        cfg.fast_budget = Duration::from_millis(ms);
    }
    if let Some(b) = v.get("warm_background").and_then(|x| x.as_bool()) {
        cfg.warm_background = b;
    }
    if let Some(arr) = v.get("unit_envs").and_then(|x| x.as_array()) {
        cfg.unit_envs = arr
            .iter()
            .filter_map(|m| m.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(b) = v.get("fast_on_stale_context").and_then(|x| x.as_bool()) {
        cfg.fast_on_stale_context = b;
    }
    if let Some(arr) = v.get("trusted_macros").and_then(|x| x.as_array()) {
        cfg.trusted_macros = arr
            .iter()
            .filter_map(|m| m.as_str().map(|s| s.to_string()))
            .collect();
    }
    match Session::open(cfg) {
        Ok(s) => Box::into_raw(Box::new(RtexSession(s))),
        Err(e) => fail(format!("{e:#}")),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rtex_session_close(s: *mut RtexSession) {
    if !s.is_null() {
        let b = Box::from_raw(s);
        b.0.close();
    }
}

/// Apply a byte-range edit. Returns the JSON `EditResult` (or {"error": ...}); free with rtex_string_free.
#[no_mangle]
pub unsafe extern "C" fn rtex_session_apply_edit(
    s: *mut RtexSession,
    path: *const c_char,
    start_byte: usize,
    end_byte: usize,
    text: *const c_char,
) -> *mut c_char {
    let Some(sess) = s.as_ref() else {
        return cstr("{\"error\":\"null session\"}".into());
    };
    let (Some(path), Some(text)) = (rstr(path), rstr(text)) else {
        return cstr("{\"error\":\"bad string\"}".into());
    };
    match sess.0.apply_edit(
        path,
        Edit {
            start_byte,
            end_byte,
            text: text.to_string(),
        },
    ) {
        Ok(r) => cstr(serde_json::to_string(&r).unwrap()),
        Err(e) => cstr(serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rtex_session_set_document(
    s: *mut RtexSession,
    path: *const c_char,
    text: *const c_char,
) -> *mut c_char {
    let Some(sess) = s.as_ref() else {
        return cstr("{\"error\":\"null session\"}".into());
    };
    let (Some(path), Some(text)) = (rstr(path), rstr(text)) else {
        return cstr("{\"error\":\"bad string\"}".into());
    };
    match sess.0.set_document(path, text) {
        Ok(r) => cstr(serde_json::to_string(&r).unwrap()),
        Err(e) => cstr(serde_json::json!({"error": e.to_string()}).to_string()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rtex_session_request_layout(s: *mut RtexSession) {
    if let Some(sess) = s.as_ref() {
        sess.0.request_layout();
    }
}

#[no_mangle]
pub unsafe extern "C" fn rtex_session_pause_background(s: *mut RtexSession, paused: bool) {
    if let Some(sess) = s.as_ref() {
        sess.0.pause_background(paused);
    }
}

#[no_mangle]
pub unsafe extern "C" fn rtex_session_export_pdf(
    s: *mut RtexSession,
    out_path: *const c_char,
) -> u64 {
    match (s.as_ref(), rstr(out_path)) {
        (Some(sess), Some(p)) => sess.0.export_pdf(p),
        _ => 0,
    }
}

/// JSON: {"versions": {...}, "convergence": ...}
#[no_mangle]
pub unsafe extern "C" fn rtex_session_status(s: *mut RtexSession) -> *mut c_char {
    let Some(sess) = s.as_ref() else {
        return cstr("{}".into());
    };
    cstr(
        serde_json::json!({"versions": sess.0.versions(), "convergence": sess.0.convergence()})
            .to_string(),
    )
}

/// Current spans of a file as JSON array.
#[no_mangle]
pub unsafe extern "C" fn rtex_session_spans(
    s: *mut RtexSession,
    path: *const c_char,
) -> *mut c_char {
    match (s.as_ref(), rstr(path)) {
        (Some(sess), Some(p)) => cstr(serde_json::to_string(&sess.0.spans(p)).unwrap()),
        _ => cstr("[]".into()),
    }
}

/// Wait up to `timeout_ms` for the next event. Returns null when none arrived.
#[no_mangle]
pub unsafe extern "C" fn rtex_session_poll(s: *mut RtexSession, timeout_ms: u32) -> *mut RtexEvent {
    let Some(sess) = s.as_ref() else {
        return std::ptr::null_mut();
    };
    let mut evs = sess.0.poll(Duration::from_millis(timeout_ms as u64));
    if evs.is_empty() {
        return std::ptr::null_mut();
    }
    // return the first; re-queue the rest by polling one at a time is not possible, so we keep
    // them in the session-side channel: poll drains, therefore hand them back in order.
    let first = evs.remove(0);
    for e in evs {
        sess.0.requeue(e);
    }
    Box::into_raw(Box::new(make_event(first)))
}

fn make_event(e: Event) -> RtexEvent {
    let (kind, dls) = match &e {
        Event::ParagraphUpdate { dl, .. } => (1, vec![dl.to_binary()]),
        Event::LayoutUpdate { pages_changed, .. } => {
            (2, pages_changed.iter().map(|p| p.dl.to_binary()).collect())
        }
        Event::Diagnostics { .. } => (3, vec![]),
        Event::EngineState { .. } => (4, vec![]),
        Event::BackgroundScheduled { .. } => (5, vec![]),
        Event::PdfExported { .. } => (6, vec![]),
    };
    // JSON without the display-list payloads (replaced by their byte sizes)
    let mut v = serde_json::to_value(&e).unwrap();
    if let Some(obj) = v.as_object_mut() {
        if let Some(dl) = obj.get_mut("dl") {
            *dl = serde_json::json!({"bytes": dls.first().map(|b| b.len()).unwrap_or(0)});
        }
        if let Some(pages) = obj.get_mut("pages_changed").and_then(|p| p.as_array_mut()) {
            for (i, p) in pages.iter_mut().enumerate() {
                if let Some(po) = p.as_object_mut() {
                    po.insert("dl".into(), serde_json::json!({"bytes": dls.get(i).map(|b| b.len()).unwrap_or(0), "index": i}));
                }
            }
        }
    }
    RtexEvent {
        kind,
        json: CString::new(v.to_string().replace('\0', " ")).unwrap(),
        dls,
    }
}

/// 1 ParagraphUpdate, 2 LayoutUpdate, 3 Diagnostics, 4 EngineState, 5 BackgroundScheduled, 6 PdfExported
#[no_mangle]
pub unsafe extern "C" fn rtex_event_kind(e: *const RtexEvent) -> u32 {
    e.as_ref().map(|e| e.kind).unwrap_or(0)
}

/// Event as JSON (display-list payloads replaced by {"bytes": n, "index": i}); valid until rtex_event_free.
#[no_mangle]
pub unsafe extern "C" fn rtex_event_json(e: *const RtexEvent) -> *const c_char {
    e.as_ref()
        .map(|e| e.json.as_ptr())
        .unwrap_or(std::ptr::null())
}

#[no_mangle]
pub unsafe extern "C" fn rtex_event_dl_count(e: *const RtexEvent) -> u32 {
    e.as_ref().map(|e| e.dls.len() as u32).unwrap_or(0)
}

/// Binary display list number `index` of the event (0 for ParagraphUpdate; pages_changed order for LayoutUpdate).
#[no_mangle]
pub unsafe extern "C" fn rtex_event_dl(
    e: *const RtexEvent,
    index: u32,
    len_out: *mut usize,
) -> *const u8 {
    let Some(ev) = e.as_ref() else {
        return std::ptr::null();
    };
    let Some(b) = ev.dls.get(index as usize) else {
        return std::ptr::null();
    };
    if !len_out.is_null() {
        *len_out = b.len();
    }
    b.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn rtex_event_free(e: *mut RtexEvent) {
    if !e.is_null() {
        drop(Box::from_raw(e));
    }
}

#[no_mangle]
pub unsafe extern "C" fn rtex_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Convert a binary display list to its JSON mirror. Free with rtex_string_free; null on error.
#[no_mangle]
pub unsafe extern "C" fn rtex_dl_to_json(bytes: *const u8, len: usize) -> *mut c_char {
    if bytes.is_null() {
        return std::ptr::null_mut();
    }
    let slice = std::slice::from_raw_parts(bytes, len);
    match DisplayList::from_binary(slice) {
        Ok(dl) => cstr(serde_json::to_string(&dl).unwrap()),
        Err(_) => std::ptr::null_mut(),
    }
}
