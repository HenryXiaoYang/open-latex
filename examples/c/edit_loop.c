/* Minimal host: open a project, wait for the first layout, apply one edit, print the paragraph
 * update. Build (after `cargo build --release -p lode-core`):
 *   cc -Iinclude examples/c/edit_loop.c -Ltarget/release -llode_core -lpthread -ldl -lm -o build/edit_loop
 *   LD_LIBRARY_PATH=target/release build/edit_loop build/fx/book-10-pure */
#include "lode.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: %s PROJECT_DIR\n", argv[0]); return 2; }
    char cfg[1024];
    snprintf(cfg, sizeof cfg, "{\"project_root\":\"%s\",\"main_file\":\"main.tex\"}", argv[1]);
    char *err = NULL;
    LodeSession *s = lode_session_open(cfg, &err);
    if (!s) { fprintf(stderr, "open failed: %s\n", err ? err : "?"); lode_string_free(err); return 1; }
    printf("lode %s\n", lode_version());
    /* wait for the first LayoutUpdate */
    for (;;) {
        LodeEvent *e = lode_session_poll(s, 1000);
        if (!e) continue;
        uint32_t k = lode_event_kind(e);
        if (k == LODE_EVENT_LAYOUT_UPDATE) {
            printf("layout: %u page display lists in event, json %zu bytes\n", lode_event_dl_count(e), strlen(lode_event_json(e)));
            lode_event_free(e);
            break;
        }
        lode_event_free(e);
    }
    /* find a body span and insert text into it */
    char *spans = lode_session_spans(s, "main.tex");
    const char *p = strstr(spans, "\"kind\":\"Body\"");
    size_t start = 0;
    if (p) {
        /* the span object looks like {"id":n,"range":{"start":a,"end":b},"kind":"Body",...}; walk back to "start" */
        const char *q = p;
        while (q > spans && strncmp(q, "\"start\":", 8) != 0) q--;
        start = (size_t) strtoul(q + 8, NULL, 10) + 10;
    }
    lode_string_free(spans);
    char *r = lode_session_apply_edit(s, "main.tex", start, start, " inserted");
    printf("edit: %s\n", r);
    lode_string_free(r);
    for (int i = 0; i < 60; i++) {
        LodeEvent *e = lode_session_poll(s, 1000);
        if (!e) continue;
        if (lode_event_kind(e) == LODE_EVENT_PARAGRAPH_UPDATE) {
            size_t len = 0;
            const uint8_t *dl = lode_event_dl(e, 0, &len);
            printf("paragraph update: %s\n", lode_event_json(e));
            printf("display list: %zu bytes, magic %c%c%c%c\n", len, dl[0], dl[1], dl[2], dl[3]);
            char *json = lode_dl_to_json(dl, len);
            printf("as json: %.120s...\n", json);
            lode_string_free(json);
            lode_event_free(e);
            break;
        }
        lode_event_free(e);
    }
    lode_session_close(s);
    return 0;
}
