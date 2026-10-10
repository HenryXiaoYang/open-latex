/* rtex — embeddable real-time LuaTeX compilation library. C ABI, rtex 0.0.2 (unstable before 0.1).
 * Strings are UTF-8. Strings returned as `char*` are owned by the caller and must be released
 * with rtex_string_free(); `const char*` results are owned by the object they came from.
 * Display lists are binary (encoding revision 1) buffers (docs/display-list.md) owned by the event. */
#ifndef RTEX_H
#define RTEX_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct RtexSession RtexSession;
typedef struct RtexEvent RtexEvent;

enum RtexEventKind {
    RTEX_EVENT_PARAGRAPH_UPDATE = 1,
    RTEX_EVENT_LAYOUT_UPDATE = 2,
    RTEX_EVENT_DIAGNOSTICS = 3,
    RTEX_EVENT_ENGINE_STATE = 4,
    RTEX_EVENT_BACKGROUND_SCHEDULED = 5,
    RTEX_EVENT_PDF_EXPORTED = 6
};

const char *rtex_version(void);

/* config_json: {"project_root":"…","main_file":"main.tex","build_dir":"…","debounce_ms":300,
 *               "max_passes":5,"fast_budget_ms":50,"compile_timeout_ms":5000,"eligibility":"probe",
 *               "trusted_macros":["…"],"unit_envs":["…"],"picture_cache":true,"warm_background":true,
 *               "fast_on_stale_context":true,"debug_dir":"…"}   (only project_root is required)
 * Returns NULL on failure and sets *err_out (free with rtex_string_free). */
RtexSession *rtex_session_open(const char *config_json, char **err_out);
void rtex_session_close(RtexSession *session);

/* Returns JSON EditResult {"edit_id","source_revision","outcome":{…},"routed":"fast|background|preamble","reasons":[…]}
 * or {"error":"…"}. */
char *rtex_session_apply_edit(RtexSession *session, const char *path, size_t start_byte, size_t end_byte, const char *text);
char *rtex_session_set_document(RtexSession *session, const char *path, const char *text);
void rtex_session_request_layout(RtexSession *session);
/* Defer background passes while true (fast path keeps working); pending passes run on resume. */
void rtex_session_pause_background(RtexSession *session, bool paused);
uint64_t rtex_session_export_pdf(RtexSession *session, const char *out_path);
char *rtex_session_status(RtexSession *session);              /* {"versions":{…},"convergence":{…}} */
char *rtex_session_spans(RtexSession *session, const char *path); /* JSON array of spans */

/* Waits up to timeout_ms for the next event; NULL when none. Events arrive in order. */
RtexEvent *rtex_session_poll(RtexSession *session, uint32_t timeout_ms);
uint32_t rtex_event_kind(const RtexEvent *event);
/* Event JSON (see docs/embedding.md); display-list payloads are replaced by {"bytes":n,"index":i}. */
const char *rtex_event_json(const RtexEvent *event);
uint32_t rtex_event_dl_count(const RtexEvent *event);
/* Binary display list `index` (0 for ParagraphUpdate; pages_changed order for LayoutUpdate). */
const uint8_t *rtex_event_dl(const RtexEvent *event, uint32_t index, size_t *len_out);
void rtex_event_free(RtexEvent *event);

char *rtex_dl_to_json(const uint8_t *bytes, size_t len);
void rtex_string_free(char *s);

#ifdef __cplusplus
}
#endif
#endif
