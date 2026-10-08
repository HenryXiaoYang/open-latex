//! Deterministic fixture generator: books of a requested page count.
//!
//! `pure`  — only fast-path-eligible body paragraphs (text, font switches, inline math).
//! `mixed` — adds chapters/sections, labels/refs, footnotes, floats, display math, lists, citations.
//! `units` — every fast-path unit kind in rotation (display math inside paragraphs, footnotes,
//!           refs, lists, quotes, theorems, figures with images, tables, headings), plain bibtex.
//! Word counts are calibrated for the `book` class at 11pt with TeX Gyre Pagella (≈ 365 words/page).

use std::fmt::Write as _;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Variant {
    Pure,
    Mixed,
    Units,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum FontSet {
    /// fontspec + TeX Gyre Pagella (OpenType, luaotfload node mode)
    Pagella,
    /// fontspec + TeX Gyre Pagella with Renderer=Basic (luaotfload base mode: engine-native ligatures/kerns)
    PagellaBase,
    /// fontspec + TeX Gyre Pagella with Renderer=HarfBuzz
    PagellaHarf,
    /// fontspec + Latin Modern (OpenType, node mode)
    LatinModern,
    /// Latin Modern via TFM/Type1 (T1 fontenc + lmodern, no fontspec): the paper's kind of preamble
    LmTfm,
}

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
    pub fn chance(&mut self, per_mille: u64) -> bool {
        self.below(1000) < per_mille
    }
}

const WORDS: &[&str] = &[
    "paragraph", "line", "breaking", "engine", "glyph", "position", "measure", "document", "page",
    "layout", "font", "kerning", "margin", "protrusion", "expansion", "editor", "keystroke",
    "latency", "cache", "background", "convergence", "footnote", "float", "counter", "reference",
    "typesetting", "algorithm", "demerits", "penalty", "glue", "stretch", "shrink", "baseline",
    "height", "depth", "width", "box", "list", "node", "attribute", "callback", "process",
    "persistent", "session", "revision", "snapshot", "render", "display", "export", "quality",
    "typography", "author", "book", "chapter", "section", "sentence", "word", "letter", "space",
    "the", "a", "an", "of", "in", "on", "with", "and", "or", "but", "for", "to", "from", "by",
    "that", "which", "when", "while", "because", "although", "every", "each", "some", "many",
    "is", "are", "was", "were", "becomes", "remains", "depends", "produces", "requires", "allows",
    "quickly", "slowly", "exactly", "nearly", "always", "never", "often", "rarely", "again",
    "fidelity", "identical", "independent", "local", "global", "incremental", "immediate",
];

const MATH: &[&str] = &[
    r"$f(x) = x^2 + 1$", r"$\sum_{i=1}^{n} a_i$", r"$\alpha + \beta = \gamma$", r"$O(1)$", r"$O(n)$",
    r"$\frac{1}{2}$", r"$x \in \mathbb{R}$", r"$\sqrt{2}$", r"$\pi \approx 3.14159$", r"$e^{i\pi} + 1 = 0$",
    r"$\lim_{n \to \infty} x_n$", r"$a \leq b$", r"$\partial f / \partial x$", r"$\mathbf{v} \cdot \mathbf{w}$",
];

fn sentence(rng: &mut Rng, pure_math_ok: bool) -> String {
    let n = rng.range(7, 18) as usize;
    let mut s = String::new();
    for i in 0..n {
        let mut w = WORDS[rng.below(WORDS.len() as u64) as usize].to_string();
        if i == 0 {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                w = f.to_uppercase().collect::<String>() + c.as_str();
            }
        }
        if i > 0 {
            s.push(' ');
        }
        if pure_math_ok && rng.chance(25) {
            s.push_str(MATH[rng.below(MATH.len() as u64) as usize]);
        } else if rng.chance(30) {
            let _ = write!(s, "\\emph{{{}}}", w);
        } else if rng.chance(12) {
            let _ = write!(s, "\\textbf{{{}}}", w);
        } else if rng.chance(8) {
            let _ = write!(s, "\\textsc{{{}}}", w);
        } else {
            s.push_str(&w);
        }
        if i + 1 < n && rng.chance(80) {
            s.push(',');
        }
    }
    s.push('.');
    s
}

/// Greedy word wrap that never splits inside `$...$` or `{...}` groups.
pub fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    let mut token = String::new();
    let mut depth_brace = 0i32;
    let mut in_math = false;
    let flush = |token: &mut String, out: &mut String, col: &mut usize| {
        if token.is_empty() {
            return;
        }
        if *col > 0 && *col + 1 + token.len() > width {
            out.push('\n');
            *col = 0;
        } else if *col > 0 {
            out.push(' ');
            *col += 1;
        }
        out.push_str(token);
        *col += token.len();
        token.clear();
    };
    for ch in text.chars() {
        match ch {
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '$' => in_math = !in_math,
            _ => {}
        }
        if ch == ' ' && depth_brace == 0 && !in_math {
            flush(&mut token, &mut out, &mut col);
        } else {
            token.push(ch);
        }
    }
    flush(&mut token, &mut out, &mut col);
    out
}

/// A one-line paragraph (4-7 plain words), the paper's "short" category.
pub fn short_paragraph(rng: &mut Rng) -> (String, usize) {
    let n = rng.range(4, 7) as usize;
    let mut s = String::new();
    for i in 0..n {
        let mut w = WORDS[rng.below(WORDS.len() as u64) as usize].to_string();
        if i == 0 {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                w = f.to_uppercase().collect::<String>() + c.as_str();
            }
        }
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&w);
    }
    s.push('.');
    (s, n)
}

/// A body paragraph. Returns (text, word_estimate).
pub fn paragraph(rng: &mut Rng, sentences: u64, math: bool) -> (String, usize) {
    let mut p = String::new();
    let mut words = 0;
    for i in 0..sentences {
        if i > 0 {
            p.push(' ');
        }
        let s = sentence(rng, math);
        words += s.split_whitespace().count();
        p.push_str(&s);
    }
    (p, words)
}

pub fn preamble(fonts: FontSet, mixed: bool) -> String {
    preamble_for(fonts, if mixed { Variant::Mixed } else { Variant::Pure })
}

pub fn preamble_for(fonts: FontSet, variant: Variant) -> String {
    let mixed = variant == Variant::Mixed;
    let units = variant == Variant::Units;
    let mut s = String::new();
    s.push_str("% Generated by `rtex gen-book`; do not edit by hand.\n");
    s.push_str("\\documentclass[11pt]{book}\n");
    match fonts {
        FontSet::LmTfm => {
            // the paper's kind of setup: TFM metrics, Type1 fonts, microtype, no fontspec
            s.push_str("\\usepackage[T1]{fontenc}\n\\usepackage{lmodern}\n\\usepackage{microtype}\n\\usepackage{amsmath}\n\\usepackage{amssymb}\n");
        }
        _ => {
            s.push_str("\\usepackage{fontspec}\n");
            s.push_str(match fonts {
                FontSet::Pagella => "\\setmainfont{TeX Gyre Pagella}\n",
                FontSet::PagellaBase => "\\setmainfont{TeX Gyre Pagella}[Renderer=Basic]\n",
                FontSet::PagellaHarf => "\\setmainfont{TeX Gyre Pagella}[Renderer=HarfBuzz]\n",
                FontSet::LatinModern => "\\setmainfont{Latin Modern Roman}\n",
                FontSet::LmTfm => unreachable!(),
            });
            // unicode-math keeps every font OpenType (no Type1 CM math), which hosts and our
            // rasterizer can render from glyph indices alone.
            s.push_str("\\usepackage{microtype}\n\\usepackage{amsmath}\n\\usepackage{unicode-math}\n");
            s.push_str(match fonts {
                FontSet::LatinModern => "\\setmathfont{Latin Modern Math}\n",
                _ => "\\setmathfont{TeX Gyre Pagella Math}\n",
            });
        }
    }
    s.push_str("\\usepackage{xcolor}\n");
    if mixed {
        s.push_str("\\usepackage{graphicx}\n\\usepackage[backend=biber,style=numeric]{biblatex}\n\\addbibresource{refs.bib}\n");
    }
    if units {
        s.push_str("\\usepackage{graphicx}\n\\usepackage{booktabs}\n\\usepackage{amsthm}\n\\newtheorem{theorem}{Theorem}[chapter]\n");
    }
    // Deterministic PDF output: suppress all optional info (incl. /ID, dates, producer).
    s.push_str("\\pdfvariable suppressoptionalinfo 1023\\relax\n");
    s.push_str("\\begin{document}\n");
    s
}

pub fn generate(pages: u32, variant: Variant, fonts: FontSet, seed: u64, out: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(out)?;
    let mut rng = Rng::new(seed ^ (pages as u64) << 8 ^ (variant as u64));
    if variant == Variant::Units {
        return generate_units(pages, fonts, seed, out, &mut rng);
    }
    let mixed = variant == Variant::Mixed;
    // calibrated on TeX Gyre Pagella 11pt book class: headings, floats, lists and the TOC of the
    // mixed variant take roughly a third of the pages
    // measured: pure ≈ 392 words/page; mixed ≈ 300 words/page plus ≈ 3 pages of front matter
    let target_words = if mixed { (pages as usize * 300).saturating_sub(1100).max(600) } else { pages as usize * 392 };
    let mut body = String::new();
    let mut words = 0usize;
    let mut chapter = 0;
    let mut labels: Vec<String> = Vec::new();
    let mut fig = 0;
    let mut para_idx = 0;
    while words < target_words {
        if mixed && (para_idx == 0 || rng.chance(40)) {
            chapter += 1;
            let _ = writeln!(body, "\\chapter{{Chapter {}}}\\label{{ch:{}}}\n", chapter, chapter);
            labels.push(format!("ch:{}", chapter));
            words += 60; // heading + whitespace cost
        } else if mixed && rng.chance(120) {
            let _ = writeln!(body, "\\section{{Section {}}}\n", para_idx);
            words += 25;
        }
        // paragraph length mix so that every benchmark category exists: ~12 % one-line
        // paragraphs (short), ~12 % 12-16-sentence paragraphs (long), the rest 2-9 sentences
        let roll = rng.below(100);
        let sentences = if roll < 12 { 1 } else if roll < 24 { rng.range(12, 16) } else { rng.range(2, 9) };
        let (mut p, w) = if sentences == 1 { short_paragraph(&mut rng) } else { paragraph(&mut rng, sentences, true) };
        words += w;
        if mixed {
            if rng.chance(80) {
                p.push_str(" As shown in Section~\\ref{");
                p.push_str(&labels[rng.below(labels.len() as u64) as usize]);
                p.push_str("}, this holds.");
            }
            if rng.chance(60) {
                p.push_str("\\footnote{A footnote with a short remark about the preceding sentence.}");
            }
            if rng.chance(40) {
                p.push_str(" See~\\cite{knuth1981}.");
            }
        }
        let _ = writeln!(body, "{}\n", wrap(&p, 78));
        para_idx += 1;
        if mixed {
            if rng.chance(50) {
                let _ = writeln!(body, "\\[ \\int_0^1 x^{} \\, dx = \\frac{{1}}{{{}}} \\]\n", para_idx % 7 + 1, para_idx % 7 + 2);
            }
            if rng.chance(40) {
                let _ = writeln!(body, "\\begin{{itemize}}\n\\item First item of a list.\n\\item Second item, slightly longer than the first one.\n\\item Third.\n\\end{{itemize}}\n");
                words += 30;
            }
            if rng.chance(35) {
                fig += 1;
                if fig % 2 == 0 {
                    let _ = writeln!(
                        body,
                        "\\begin{{figure}}[tbp]\\centering\\includegraphics[width=0.5\\textwidth]{{images/figure.png}}\\caption{{Figure {} (image).}}\\label{{fig:{}}}\\end{{figure}}\n",
                        fig, fig
                    );
                } else {
                    let _ = writeln!(
                        body,
                        "\\begin{{figure}}[tbp]\\centering\\rule{{0.6\\textwidth}}{{3cm}}\\caption{{Figure {} placeholder.}}\\label{{fig:{}}}\\end{{figure}}\n",
                        fig, fig
                    );
                }
                words += 120;
            }
            if rng.chance(30) {
                let _ = writeln!(body, "A \\textcolor{{red}}{{colored}} word and {{\\color{{blue}}a blue phrase}} inside a paragraph that also has a footnote.\\footnote{{Footnote text for the colored paragraph.}}\n");
                words += 20;
            }
        }
    }
    let mut main = preamble(fonts, mixed);
    if mixed {
        main.push_str("\\tableofcontents\n\n");
    }
    main.push_str(&body);
    if mixed {
        main.push_str("\\printbibliography\n");
    }
    main.push_str("\\end{document}\n");
    std::fs::write(out.join("main.tex"), main)?;
    if mixed {
        std::fs::create_dir_all(out.join("images"))?;
        std::fs::write(out.join("images").join("figure.png"), png_placeholder())?;
        std::fs::write(
            out.join("refs.bib"),
            "@article{knuth1981,\n  author = {Donald E. Knuth and Michael F. Plass},\n  title = {Breaking paragraphs into lines},\n  journal = {Software: Practice and Experience},\n  year = {1981},\n  volume = {11},\n  number = {11},\n  pages = {1119--1184}\n}\n",
        )?;
    }
    std::fs::write(
        out.join("fixture.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "pages_requested": pages, "variant": format!("{:?}", variant).to_lowercase(),
            "fonts": format!("{:?}", fonts), "seed": seed, "paragraphs": para_idx, "words": words,
        }))?,
    )?;
    Ok(())
}

/// The `units` variant: a rotation of every unit kind the fast path supports, so each kind
/// appears many times in a 10-page book and can be verified and benchmarked.
fn generate_units(pages: u32, fonts: FontSet, seed: u64, out: &Path, rng: &mut Rng) -> anyhow::Result<()> {
    // measured: ≈ 230 words of body text per page once the structural units are counted in
    let target_words = (pages as usize * 230).max(500);
    let mut body = String::new();
    let mut words = 0usize;
    let mut chapter = 0;
    let mut eq = 0;
    let mut fig = 0;
    let mut tab = 0;
    let mut thm = 0;
    let mut para_idx = 0;
    let mut labels: Vec<String> = Vec::new();
    while words < target_words {
        if para_idx % 9 == 0 {
            chapter += 1;
            let _ = writeln!(body, "\\chapter{{Chapter {}}}\\label{{ch:{}}}\n", chapter, chapter);
            labels.push(format!("ch:{chapter}"));
            words += 60;
        } else if para_idx % 3 == 0 {
            let _ = writeln!(body, "\\section{{Section {}}}\\label{{sec:{}}}\n", para_idx, para_idx);
            labels.push(format!("sec:{para_idx}"));
            words += 25;
        }
        let roll = rng.below(100);
        let sentences = if roll < 12 { 1 } else if roll < 24 { rng.range(10, 14) } else { rng.range(2, 7) };
        let (mut p, w) = if sentences == 1 { short_paragraph(rng) } else { paragraph(rng, sentences, true) };
        words += w;
        // one structural feature per paragraph, in rotation
        match para_idx % 10 {
            0 => {
                // plain
            }
            1 => {
                eq += 1;
                p.push_str(&format!(" The identity\n\\begin{{equation}} \\int_0^1 x^{} \\, dx = \\frac{{1}}{{{}}} \\label{{eq:{}}} \\end{{equation}}\nholds for every exponent, and we use it below.", eq % 7 + 1, eq % 7 + 2, eq));
                labels.push(format!("eq:{eq}"));
            }
            2 => {
                p.push_str(" The displayed sum\n\\[ \\sum_{i=1}^{n} i = \\frac{n(n+1)}{2} \\]\nis classical.");
            }
            3 => {
                p.push_str("\\footnote{A footnote with a short remark about the preceding sentence.} The text goes on after the footnote mark.");
            }
            4 => {
                if !labels.is_empty() {
                    p.push_str(&format!(" As shown in Section~\\ref{{{}}} on page~\\pageref{{{}}}, this holds; see also~\\cite{{knuth1981}}.", labels[rng.below(labels.len() as u64) as usize], labels[0]));
                }
                if eq > 0 {
                    p.push_str(&format!(" Equation~\\eqref{{eq:{}}} is used here.", eq));
                }
            }
            5 => {
                eq += 2;
                p.push_str(&format!(" Two aligned relations\n\\begin{{align}} a_{} &= b + c \\label{{eq:{}}} \\\\ d &= \\sqrt{{e^2 + f^2}} \\label{{eq:{}}} \\end{{align}}\nfinish the paragraph.", eq, eq - 1, eq));
            }
            _ => {}
        }
        let _ = writeln!(body, "{}\n", wrap(&p, 78));
        match para_idx % 10 {
            6 => {
                let _ = writeln!(body, "\\begin{{itemize}}\n\\item First item of a list, with a few words so that it is not trivial.\n\\item Second item, with inline math $x_{} + y$ in it.\n\\item Third.\n\\end{{itemize}}\n", para_idx);
                words += 30;
            }
            7 => {
                fig += 1;
                let _ = writeln!(body, "\\begin{{figure}}[tbp]\n\\centering\n\\includegraphics[width=0.4\\textwidth]{{images/figure.png}}\n\\caption{{Figure {} shows the placeholder image with a caption long enough to wrap onto a second line of the caption block.}}\\label{{fig:{}}}\n\\end{{figure}}\n", fig, fig);
                words += 110;
            }
            8 => {
                tab += 1;
                let _ = writeln!(body, "\\begin{{table}}[tbp]\n\\centering\n\\caption{{Table {} with three columns.}}\\label{{tab:{}}}\n\\begin{{tabular}}{{lrr}}\n\\toprule\nName & Value & Count \\\\\n\\midrule\nalpha & {} & 3 \\\\\nbeta & {} & 12 \\\\\ngamma & {} & 7 \\\\\n\\bottomrule\n\\end{{tabular}}\n\\end{{table}}\n", tab, tab, tab * 11, tab * 7, tab * 5);
                words += 90;
            }
            9 => {
                thm += 1;
                if thm % 2 == 1 {
                    let _ = writeln!(body, "\\begin{{theorem}}\\label{{thm:{}}}\nFor every $n \\geq 1$ the sum of the first $n$ odd numbers is $n^2$, and the statement is long enough to wrap.\n\\end{{theorem}}\n", thm);
                } else {
                    let _ = writeln!(body, "\\begin{{quote}}\nA quotation set with the quote environment, indented on both sides and long enough to need a second line of text.\n\\end{{quote}}\n");
                }
                words += 40;
            }
            _ => {}
        }
        para_idx += 1;
    }
    let mut main = preamble_for(fonts, Variant::Units);
    main.push_str("\\tableofcontents\n\n");
    main.push_str(&body);
    main.push_str("\\bibliographystyle{plain}\n\\bibliography{refs}\n");
    main.push_str("\\end{document}\n");
    std::fs::write(out.join("main.tex"), main)?;
    std::fs::create_dir_all(out.join("images"))?;
    std::fs::write(out.join("images").join("figure.png"), png_placeholder())?;
    std::fs::write(
        out.join("refs.bib"),
        "@article{knuth1981,\n  author = {Donald E. Knuth and Michael F. Plass},\n  title = {Breaking paragraphs into lines},\n  journal = {Software: Practice and Experience},\n  year = {1981},\n  volume = {11},\n  number = {11},\n  pages = {1119--1184}\n}\n",
    )?;
    std::fs::write(
        out.join("fixture.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "pages_requested": pages, "variant": "units", "fonts": format!("{:?}", fonts), "seed": seed,
            "paragraphs": para_idx, "words": words, "equations": eq, "figures": fig, "tables": tab,
        }))?,
    )?;
    Ok(())
}

/// A deterministic 64×40 RGB PNG (diagonal gradient) written without external crates.
fn png_placeholder() -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            }
            table[i as usize] = c;
        }
        let mut crc = 0xFFFFFFFFu32;
        for &b in data {
            crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
        crc ^ 0xFFFFFFFF
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &d in data {
            a = (a + d as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    }
    let (w, h) = (64usize, 40usize);
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0u8); // filter none
        for x in 0..w {
            raw.push((x * 4) as u8);
            raw.push((y * 6) as u8);
            raw.push(((x + y) * 2) as u8);
        }
    }
    // zlib stream with stored (uncompressed) deflate blocks
    let mut z = vec![0x78, 0x01];
    let mut i = 0;
    while i < raw.len() {
        let n = (raw.len() - i).min(65535);
        let last = if i + n == raw.len() { 1u8 } else { 0u8 };
        z.push(last);
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[i..i + n]);
        i += n;
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
