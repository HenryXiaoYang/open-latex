# Changelog

## 0.0.2 (in progress)

- Fast-path latency: raw-framed compile requests, `\luafunction` dispatch in the server, one
  state fingerprint per compile, bounded busy-poll for replies, direct submission from
  `apply_edit` when the server is idle, lazy context upload, windowed re-segmentation next to the
  preamble. Short-paragraph round trip through a session: 0.63 ms → 0.40 ms on the reference
  container (docs/BENCHMARKS.md).
- `lode probe`: latency breakdown of the fast path (direct server, in-engine profile, session).
- Fast path generalized from paragraphs to **units**: paragraphs with display math (`\[ \]`,
  `equation`, amsmath `align`/`gather`/`multline`), footnote marks, `\ref`/`\eqref`/`\pageref`/
  `\cite`/`\label`; block environments (lists, `quote`, `center`, theorem-like, `figure`/`table`
  with `\includegraphics`, `tabular`, `booktabs`, `\caption`); headings. Counters, kernel
  switches, `\everypar` patterns and float-box state are captured and replayed; aux labels are
  uploaded to the server; images are mapped by resource index. Rows (hlists reached through
  vlists) replace lines as the unit of placement; fragments carry per-row positions anchored at
  each page's first row. A per-unit fast budget (5 ms) routes slow units to the background path.
- Display list: `LINE_UNIT`, `IMAGE_INFO` and `MATRIX` records (graphicx scaling is now exact,
  not degraded); unit lists carry an insert count.
- `lode gen-book --variant units` fixture; `lode verify` works per unit kind with `--units` and
  `--dump-rows`.
- Versioning reset: the library, protocol and documents are versioned together as 0.0.x; nothing
  is frozen before 0.1.

## 0.0.1

- Initial release: persistent LuaTeX paragraph server, capture package, display list (binary
  encoding revision 1 + JSON mirror), Rust core with C ABI, background passes with convergence
  contract, PDF export, fidelity verification, fixtures and benchmarks (milestones M0–M7).
