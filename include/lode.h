/* lode — embeddable real-time LuaTeX compilation library. C ABI, lode 0.0.2 (unstable before 0.1).
 * Strings are UTF-8. Strings returned as `char*` are owned by the caller and must be released
 * with lode_string_free(); `const char*` results are owned by the object they came from.
 * Display lists are binary (encoding revision 1) buffers (docs/DISPLAY_LIST.md) owned by the event. */
#ifndef LODE_H
#define LODE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct LodeSession LodeSession;
typedef struct LodeEvent LodeEvent;

enum LodeEventKind {
    LODE_EVENT_PARAGRAPH_UPDATE = 1,
    LODE_EVENT_LAYOUT_UPDATE = 2,
    LODE_EVENT_DIAGNOSTICS = 3,
    LODE_EVENT_ENGINE_STATE = 4,
    LODE_EVENT_BACKGROUND_SCHEDULED = 5,
    LODE_EVENT_PDF_EXPORTED = 6
};

const char *lode_version(void);

/* config_json: {"project_root":"…","main_file":"main.tex","build_dir":"…","debounce_ms":300,
 *               "max_passes":5,"trusted_macros":["…"],"fast_on_stale_context":true,"compile_timeout_ms":5000}
 * Returns NULL on failure and sets *err_out (free with lode_string_free). */
LodeSession *lode_session_open(const char *config_json, char **err_out);
void lode_session_close(LodeSession *session);

/* Returns JSON EditResult {"edit_id","source_revision","outcome":{…},"routed":"fast|background|preamble","reasons":[…]}
 * or {"error":"…"}. */
char *lode_session_apply_edit(LodeSession *session, const char *path, size_t start_byte, size_t end_byte, const char *text);
char *lode_session_set_document(LodeSession *session, const char *path, const char *text);
void lode_session_request_layout(LodeSession *session);
/* Defer background passes while true (fast path keeps working); pending passes run on resume. */
void lode_session_pause_background(LodeSession *session, bool paused);
uint64_t lode_session_export_pdf(LodeSession *session, const char *out_path);
char *lode_session_status(LodeSession *session);              /* {"versions":{…},"convergence":{…}} */
char *lode_session_spans(LodeSession *session, const char *path); /* JSON array of spans */

/* Waits up to timeout_ms for the next event; NULL when none. Events arrive in order. */
LodeEvent *lode_session_poll(LodeSession *session, uint32_t timeout_ms);
uint32_t lode_event_kind(const LodeEvent *event);
/* Event JSON (see docs/API.md); display-list payloads are replaced by {"bytes":n,"index":i}. */
const char *lode_event_json(const LodeEvent *event);
uint32_t lode_event_dl_count(const LodeEvent *event);
/* Binary display list `index` (0 for ParagraphUpdate; pages_changed order for LayoutUpdate). */
const uint8_t *lode_event_dl(const LodeEvent *event, uint32_t index, size_t *len_out);
void lode_event_free(LodeEvent *event);

char *lode_dl_to_json(const uint8_t *bytes, size_t len);
void lode_string_free(char *s);

#ifdef __cplusplus
}
#endif
#endif
