# Eligibility: allow-list today, validation by comparison as the universal alternative

Two ideas were tested in October 2026: whether the per-unit fast budget should be a parameter
(yes; it is `fast_budget_ms`, see the end), and whether the fast path needs an allow-list of
LaTeX commands at all, or whether one universal mechanism can decide for any construct.

## Why there is an allow-list

A fast compile typesets one unit inside a persistent LuaTeX. It is exact only if (a) the unit
leaves no state behind that would change later compiles, (b) its output depends on nothing
the context replay does not restore, (c) it finishes within the budget, and (d) the display
list can represent its output. The allow-list (`crates/rtex-core/src/eligibility.rs`) proves
(a) and (b) *by construction*: every listed macro is known to be pure or to depend only on
captured state. (c) and (d) are already universal: the budget and the watchdog bound time, and
the traversal flags anything it cannot represent (`ok_degraded`, PDF fallback for the page).

The price is that every unlisted command, package macro or environment is background-only
until someone adds it, which is the maintenance burden this note is about.

## Experiment 1: a universal leak barrier (`\globaldefs=-1`) — fails

TeX has a primitive switch: with `\globaldefs < 0` every assignment is local, `\global` or not.
Setting it inside the unit's box group would undo *any* assignment at the end of the compile,
making (a) universal. Tried in `rtex-serve.lua`'s head tokens.

Result: LaTeX's own internals publish results through `\begingroup … \global … \endgroup`
(NFSS defines fonts that way, enumitem's key parser, hyperref, `array`). With the switch on,
headings lost their size (`Missing font identifier OT1/lmr/m/n/10.95`), enumerate and tables
failed to compile, and 17 of 17 units of the `cs` corpus differed. Not usable. The fingerprint
(group level, nest, catcodes, `\count0–9`, `\everypar`, `\hsize`, `\parindent`, kernel
conditionals) plus the LaTeX counter restore remain the leak defence.

## Experiment 2: ignore the allow-list and let the comparison judge — works

`rtex verify --permissive` keeps only the *structural* reasons (paragraph breaks, unbalanced
groups, text after a block environment, missing context, shape mismatch) and drops every
"unknown macro / environment / package" reason, then compares the fast result of each unit
row by row with the layout pass as usual. `fixtures/research/permissive/main.tex` holds 28
units built from constructs the allow-list rejects today.

| Construct | Fast result vs layout |
|---|---|
| inline `\tikz`, a `tikzpicture` in `center` | identical glyphs (paths are `LITERAL` records: degraded page, as today) |
| `\scalebox`, `\rotatebox`, `\resizebox` | identical |
| `\MakeUppercase`, `\MakeLowercase`, `\uppercase` | identical |
| primitives: `\kern`, `\hskip`, `\llap`, `\rlap`, `\smash`, `\hbox`, `\lower`, `\raise`, `\penalty` | identical |
| `\lipsum[1][1-2]` inside a paragraph | identical |
| textcomp symbols (`\textbullet`, `\textcelsius`, `\textrightarrow`, …) | identical |
| `\thepage`, `\arabic{page}` | identical **on page 1 only** (page counter is never replayed): a genuine dependence the comparison catches wherever it matters |
| `\marginpar` | **differs** (the capture counts the note's rows with the unit; the box does not) |
| `\newcommand`/`\def` inside the paragraph, used in it | identical (the definition leaks into the server; harmless here) |
| `\pagecolor` | identical rows (the leak is a global page colour no box shows) |
| `\hypertarget`/`\hyperlink`/`\href` | identical |
| `\boxed`, `\pmb`, `\overset`, `\underset`, `\xrightarrow`, accents, `\mathchoice`, explicit styles | identical |
| `framed`, `sloppypar`, `samepage`, `tabbing`, `minipage` with a footnote | identical |
| `multicols` | **differs** (page builder output; 1 row vs 7) |
| colour models (`[rgb]`, `[gray]`, `[HTML]`, `teal!60!black`) | identical |
| `\vskip`/`\vspace` inside a paragraph, `\newline`, `\\*` | identical |
| `\footnotemark`/`\footnotetext`, `\centerline`, `\char`/`\symbol`/`\string`, nested `\emph`, custom `\item[...]` labels, `\loop`, `\foreach` | identical |

28 eligible of 29 units (one unmapped), 26 exact, 2 different, 2269 of 2305 glyphs identical.
On the seven corpus documents, permissive mode admits exactly one unit the allow-list rejects
(the `counters` paragraph whose `\stepcounter` precedes its text) and the comparison flags it:
the equation number comes out one too high, as `LeadingCounter` predicts; everything else is
unchanged.
Every difference is detected by the row comparison, and the two state-dependent cases
(`\thepage`, `\marginpar`) are exactly the ones no allow-list entry could have made safe.

## What this means: validation by comparison is the universal mechanism

The allow-list answers "could this construct ever go wrong?"; the comparison answers "does
this unit, as it stands in this document, come out exactly as the layout pass typeset it?".
The second question is the one that matters for the fast path, and it can be asked at run
time for every unit, for any construct, with data the session already has:

1. **Structural pre-filter** (kept): paragraph breaks, unbalanced groups, block boundaries,
   setup spans, missing context, shape mismatch. These are about the unit's extent, not its
   vocabulary, and they stay.
2. **Probe compile**: the first time a unit is edited after a layout, compile its *snapshot
   text* (the text the pass typeset) once and compare the rows with the capture's placements
   (row count, each row's width/height/depth and glue set, glyph count per row). Match →
   the unit is *verified* for this layout and its edits are live; mismatch or error → the
   unit is background-only until the next layout, with the reason reported
   (`Unverified`). One extra compile of 1–5 ms per unit per layout, paid lazily.
3. **Existing universal guards stay**: the fingerprint and counter restore (leaks), the
   budget and watchdog (time), the display-list flags (representability), the advanced-counter
   report (renumbering), the discard rules (stale results).
4. **The allow-list becomes an optimisation**, not a gate: a unit whose vocabulary is
   allow-listed skips the probe (its exactness is proven by construction); everything else is
   probed. Residual risk is the probe's blind spot: an edit that introduces a dependence the
   snapshot text did not have (typing `\thepage` into a verified paragraph on page 2). The
   next layout pass re-verifies and demotes it, exactly as the budget demotes slow units today.

Leaks remain the one class the comparison does not see directly (a unit that redefines a macro
another unit uses). Two cheap additions close most of it when probing is on: compare the
meanings of every control sequence the unit's source mentions before and after the compile,
and extend the fingerprint to all `\count`/`\dimen`/`\skip` registers 0–255.

## Probe mode (implemented; the default)

`SessionConfig::eligibility` (C ABI JSON `"eligibility": "probe" | "allowlist"`, CLI
`rtex serve --eligibility`) selects the mechanism; `probe` is the default.

In probe mode `classify_source` keeps only the structural reasons. When a unit's vocabulary
has reasons the allow-list would reject, the edit is routed as "fast" and its request carries
the span's **snapshot text** (recorded with every layout). The engine thread compiles that text
first and `LayoutStore::probe_check` compares the result with the pass's rows (row count,
each row's box and glue set, every glyph's font, char, position, width and expansion — the
same comparison `rtex verify` runs). A match verifies the unit for this layout; its request
is then sent and every later edit skips the probe. A mismatch, a compile error or a leak
demotes it: `BackgroundScheduled { reasons: ["unverified: …"] }`, a layout pass is scheduled,
and until the next layout `apply_edit` routes the unit to the background up front with the
same reason. Verdicts are cleared when a layout is installed. Allow-listed units never probe.

**Leaks.** The server records the meaning of every distinct control sequence a unit's source
mentions before the compile and compares after it; names whose meaning changed are reported as
`leaks` in the result (`\gdef`, `\global\let`; a `\newcommand` inside a paragraph is local to
the unit's box group and is *not* a leak). A leak demotes the unit until the preamble changes
and restarts the engine, because the server's state is no longer the document's; this is
checked before any other handling of a result, warm-ups and superseded results included. A
name built with `\csname…\endcsname` does not appear in the source and is not seen: the
fingerprint and the counter restore remain the defence for that case.

**Bookkeeping.** The probe runs on the engine thread without the link lock held (the host's
`apply_edit` never waits on it; the direct dispatch path stays off the server meanwhile), it
reads the snapshot text from the layout current at that moment and judges against that same
layout (a request whose layout moved during the probe goes back to the queue), and verdicts
are keyed by layout version. A demotion after `apply_edit` reported "fast" removes the unit's
overlay and counts as a background change for the spans after it, like a synchronous
background routing would. `LayoutUpdate.eligible_paragraphs` lists every structurally eligible
unit, probed or not; the warm-up compile only ever picks an allow-listed one.

Tests: `probe_mode_verifies_and_demotes` (session) walks the three outcomes on one document
(`\scalebox` verified live; `\thepage` on page 2 demoted with the glyph difference; a `\gdef`
demoted with a restart); `leaked_definitions_are_reported` (engine). CI runs
`rtex verify --permissive` on the research fixture (`--min-eligible 28 --expect-differing 2`:
exactly the two state-dependent units must be caught, neither more nor fewer) and on the
corpus, so a construct the comparison stops catching, or one it starts rejecting, fails the
build.

Cost: one extra compile (1–5 ms) per not-allow-listed unit per layout, on its first edit.

## The fast budget is a parameter

`fast_budget_ms` (default 5): a unit whose fast compiles exceed it three times in a row goes to
the background path until the next layout. Session config `SessionConfig::fast_budget`, C ABI
JSON `"fast_budget_ms"`, CLI `rtex serve --fast-budget-ms`. TikZ-style units would want a
larger value (a three-node flowchart costs about 17 ms in the engine, a 100-sample plot
about 56 ms): the budget is per session for now.
