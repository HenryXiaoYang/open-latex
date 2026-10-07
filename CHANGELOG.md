# Changelog

## 0.0.2 (in progress)

- Fast-path latency: raw-framed compile requests, `\luafunction` dispatch in the server, one
  state fingerprint per compile, bounded busy-poll for replies, direct submission from
  `apply_edit` when the server is idle, lazy context upload, windowed re-segmentation next to the
  preamble. Short-paragraph round trip through a session: 0.63 ms → 0.40 ms on the reference
  container (docs/BENCHMARKS.md).
- `lode probe`: latency breakdown of the fast path (direct server, in-engine profile, session).
- Versioning reset: the library, protocol and documents are versioned together as 0.0.x; nothing
  is frozen before 0.1.

## 0.0.1

- Initial release: persistent LuaTeX paragraph server, capture package, display list (binary
  encoding revision 1 + JSON mirror), Rust core with C ABI, background passes with convergence
  contract, PDF export, fidelity verification, fixtures and benchmarks (milestones M0–M7).
