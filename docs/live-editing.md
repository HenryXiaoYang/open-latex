# Live editing: what updates instantly, and what waits

When you type, the paragraph you are in is re-typeset on its own and appears on the page within
a millisecond or so. Everything that depends on the whole document (page breaks, where floats
go, the table of contents, reference numbers) is updated by a full compile in the background.
After the first one, that compile takes a fraction of a second for short documents and a few
seconds for long ones.

This page lists what falls on which side. [how-it-works.md](how-it-works.md) explains why.

## Updates as you type

| You edit | Notes |
|---|---|
| Body text, accents, `\emph`, `\textbf` and other font switches, colours, boxes (`\fbox`, `\parbox`, `\colorbox`, `\underline`, `\rule`) | |
| Inline math and display math: `\[…\]`, `equation`, `align`, `gather`, `multline`, with `\label`, `\tag` and equation numbers | |
| `\ref`, `\eqref`, `\pageref`, `\autoref`, `\cite` (plain, natbib and biblatex) | Numbers come from the last background pass. A new label shows its number after the next pass. |
| Lists (`itemize`, `enumerate`, `description`, enumitem labels), `quote`, `quotation`, `verse`, `center`, `abstract`, theorems and `proof`, `verbatim`, `lstlisting` | Each environment is one unit. |
| Figures and tables: `\includegraphics`, `tabular`, `tabularx`, `booktabs`, `multirow`, `\rowcolor`, `subfigure`, `\caption` | Updated where the float currently is. If it should move, it moves after the next pass. |
| Headings (`\chapter`, `\section`, …) | The table of contents and running heads follow after the next pass. |
| Footnotes | The mark updates live. The footnote text at the bottom of the page follows after the next pass. |
| Title block (`\title`, `\author`, `\date`, `\maketitle`) in `article`/`report` without `titlepage` | |
| `thebibliography` with `\bibitem` | |
| `\setcounter`, `\stepcounter`, `\refstepcounter` inside a paragraph | When this changes the numbering of what follows, a pass is scheduled to renumber it. |
| Macros and environments you define (`\newcommand`, `\DeclareMathOperator`, `\newenvironment`) | Allowed when their bodies only use commands rtex knows. In the default probe mode, others are allowed too after one verification compile. |
| Package commands: hyperref (`\url`, `\href`), siunitx, ulem, soul, listings, cancel, bm, csquotes, mhchem, … | The package must be loaded in your preamble. |
| Splitting a paragraph, merging two, typing a new paragraph or list | The new part borrows its neighbour's settings until the next pass confirms them. Numbers may lag by one, and its vertical position is approximate. |
| A paragraph that grows or shrinks by a line | Its own lines update live. The text after it moves after the next pass. |

In the default **probe** mode, any unit that is sound in shape can go live, even if it uses
commands rtex has never heard of. The first time you edit it after a layout, rtex typesets its
old text once and compares the result with the page. If they match exactly, your edits go live.
If not, the unit waits for the pass and you are told why.

## Updates after the background pass

| You edit | Why it waits |
|---|---|
| The preamble (packages, settings, macro definitions) | The live engine restarts with the new preamble (about 1–2 s). |
| `\tableofcontents`, `\listoffigures`, `\printbibliography`, `\printindex` | Generated from the auxiliary files. |
| `\marginpar`, `wrapfigure`, multicolumn text, a paragraph that continues after a float in the same TeX paragraph | Placed by the page builder, not by the paragraph. |
| Setup lines on their own between paragraphs (`\newcommand`, `\setlength`, `\definecolor`, `\lstset`, …) | rtex loads them with the preamble, so macros defined there work live everywhere. Editing them is like editing the preamble: the live engine restarts. |
| `\setlength` or `\renewcommand` at the top level of a paragraph, next to its text | It changes how everything after it is typeset. Inside a group or an environment, it is fine. |
| Run-in headings (`\paragraph`, `\subparagraph`) | The heading shares its line with the following paragraph. |
| A list that continues an earlier one (enumitem `resume`, `series`) | The earlier list's count is not a LaTeX counter. |
| Paragraphs inside `tcolorbox`, text printed from Lua (`\directlua`, `luacode`) | rtex cannot attribute those lines to a source span. |
| Text whose output depends on state built up earlier in the document that rtex does not restore | The probe sees the difference, and the unit waits for the pass. |
| A unit whose live compiles keep taking longer than the budget (50 ms by default) | Usually a large TikZ picture being edited. It goes back to live editing after the next layout. |
| A unit that hung or crashed the live engine | Kept on the background path until the preamble changes, so it cannot crash the engine again. |

**TikZ, pgfplots and other drawings** are typeset live when they fit in the budget, but the
display list cannot describe vector paths. Pages that contain them are drawn from the PDF of the
last pass instead (see [Rendering](#rendering)). Unchanged pictures are reused from an earlier
pass, so they cost almost nothing.

## Making it faster

**Fonts make the biggest difference.** With fontspec's default settings, LuaTeX shapes every
paragraph in Lua, which takes several times longer than breaking it into lines. For the fastest
updates, use one of these:

```latex
\usepackage{fontspec}
\setmainfont{TeX Gyre Pagella}[Renderer=Basic]   % OpenType, engine-native shaping
```
or classic fonts:
```latex
\usepackage[T1]{fontenc}
\usepackage{lmodern}
```

`Renderer=Basic` is fine for Latin, Greek and Cyrillic text. Scripts that need complex shaping
(Arabic, Indic scripts, …) need the default renderer, and still get live updates, just slower.
See [benchmarks.md](benchmarks.md) for the numbers.

**Keep the preamble stable while you write.** Every preamble edit restarts the live engine.

**Split very long paragraphs.** The cost of a live update grows with the paragraph, not with
the document. A 10-line paragraph takes about 2 ms, a one-liner a third of a millisecond.

## Requirements

- **LuaLaTeX only.** pdfLaTeX and XeLaTeX are not supported.
- **A LaTeX kernel from 2021 or later.** rtex uses the kernel's paragraph and shipout hooks.
- **Bibliographies and indexes:** `biber`, `bibtex` and `makeindex` run automatically when the
  document needs them. With imakeidx, the `program=` and `options=` you give are used, and
  `.ist` files in the project are found. `xindy` and `makeglossaries` are not run automatically.
  glossaries' `\printnoidxglossaries` needs no external tool and works.
- **Multi-file projects:** files reached through `\input`, `\include` and `\subfile` are
  followed. A file name built from a macro (`\input{\chapterdir/x}`), `\includeonly` and
  `\import` are not followed.
- **Platforms:** Linux, macOS and Windows.

## Rendering

The live engine and the background pass describe pages as display lists: glyphs from font
files at exact positions, rules, images and colours ([display-list.md](display-list.md)). A
page is marked *degraded* when it contains something a display list cannot describe:
- vector drawings (TikZ, `\pdfliteral`);
- right-to-left text;
- a page rotated by pdflscape;
- a picture taken from the picture cache.

For degraded pages, rtex gives the host the PDF of that layout, and the host draws those pages
from the PDF.

Type 1 fonts (classic Computer Modern math without `unicode-math`) are named by file and
character code. The host must be able to draw them.

## Known differences from a full run

The live engine skips work that cannot be seen inside a paragraph. It does not record
running-head marks, it caches the math font setup per size, and it remembers which image
files exist. Each shortcut checks that the LaTeX macro it replaces still has the definition it
was written for, and is skipped otherwise. CI compares every kind of unit against a full run
that does not use these shortcuts ([correctness.md](correctness.md)).

The probe has one blind spot. Suppose you add a dependency the old text did not have to a
paragraph that was already verified, for example by typing `\thepage` into a paragraph on page
2. That paragraph shows the wrong number until the next layout, which checks it again.
