# Convergence contract

A background pass runs the instrumented full compile (`lode-capture`) on a snapshot of the
buffers, repeating while the aux family changes or the log asks for a rerun, up to
`max_passes` (default 5). `biber`/`bibtex` run between passes when the document asks for them
(`.bcf` present or `\bibdata` in the aux; `bib_tool` selects the tool or disables this).

`LayoutUpdate.convergence`:

| State | Meaning |
|---|---|
| `Converged` | aux family unchanged, no rerun request, no compile errors, and no edits since the snapshot |
| `Converging{pass, reasons}` | a further pass is needed (aux still changing, errors present) and will run |
| `PassLimitReached{passes, reasons}` | `max_passes` exhausted without stabilizing — **never reported as converged** |
| `Stale{pending_since}` | edits arrived after the snapshot; another pass is already scheduled |

`LayoutUpdate.compile`:

| State | Meaning |
|---|---|
| `Ok` | no errors in the log (warnings such as undefined references are reported as `Diagnostics`) |
| `CompiledWithErrors{count}` | pages were shipped but the log contains errors; the layout is partial output, not a converged layout |
| `Failed` | no page was produced; the previous layout stays current |

A missing bibliography tool makes the pass stop with `PassLimitReached` and a diagnostic.

`PdfExported.status` follows the same scale (`Ok` / `CompiledWithErrors` / `Failed`) and carries
`converged` (aux stable and no errors); only `converged = true` claims equality with a clean
LuaLaTeX build.
