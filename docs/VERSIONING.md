# Versioning and validity rules

Every event carries a `Versions` record:

| Field | Meaning | Bumped by |
|---|---|---|
| `source_revision` | monotonic per session | every `set_document` / `apply_edit` on any file |
| `context_revision` | which set of paragraph contexts the fast server replays | each installed background layout |
| `engine_generation` | which fast-server process produced a result | every (re)start: preamble change, watchdog kill, state-fingerprint mismatch, crash |
| `layout_version` | which `LayoutUpdate` placements/pages are current | each installed background layout |

Each span (paragraph-ish unit, see `document.rs`) stores the revision of the last edit that
touched it. The project also remembers, per file, the revisions at which a **background-only**
span (or the preamble) changed and where it was.

## Rules

1. **Fast request.** A request carries `(par_id, span_hash, source_revision, context_revision,
   engine_generation)`. When its result arrives it is **discarded** if the span's hash differs
   from the request's (a newer edit superseded it; pending requests are coalesced per paragraph,
   latest wins) or if `engine_generation` moved on. Otherwise it is delivered as
   `ParagraphUpdate` and recorded in the overlay table.
2. **Context validity.** A span's context is the engine paragraph the latest layout mapped to it
   (exactly one engine paragraph per span, by snapshot line range). It is **stale** when a
   background-only span or the preamble *before* it in the same file changed after the layout's
   snapshot revision; with `fast_on_stale_context = true` (default) results are still delivered
   with `context_stale = true`, otherwise the paragraph is background-only until the next pass.
   A span with no mapped engine paragraph has no context: background-only (`NoContext`).
3. **Background pass.** Compiled from a snapshot at `source_revision = S`; always delivered.
   If the current revision is `> S` the layout is `Stale{pending_since}` and another pass is
   scheduled. Overlays with `last_revision ≤ S` are committed (dropped); newer overlays are kept
   and re-anchored by the host using the new placements for the same `par_id`.
4. **Preamble change.** `engine_generation += 1` (the server restarts with the new preamble),
   pending requests and overlays are dropped, contexts are invalidated, a pass is scheduled.
   Hosts see `EngineState{Starting}` → `EngineState{Ready}` → `LayoutUpdate`.
5. **Boundary changes.** An edit that splits or merges spans (or touches several) removes the old
   ids and allocates new ones (`EditResult.outcome`), and routes to the background path; the new
   spans become fast-eligible when the next layout maps contexts to them.

## What hosts should do with a `ParagraphUpdate`

- Draw the display list at the `fragments` (one per page the paragraph occupies; per-line
  baselines in page coordinates), replacing everything owned by `par_id`.
- If `pagination_stale` is true the paragraph's height or line count changed: following material
  may be mispositioned until the next `LayoutUpdate` (already scheduled).
- If `context_stale` is true, something before this paragraph changed since its context was
  captured; the result is the best available and will be confirmed by the next layout.
