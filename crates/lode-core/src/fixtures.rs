//! Deterministic fixture generator: books of a requested page count.
//!
//! `pure`  — only fast-path-eligible body paragraphs (text, font switches, inline math).
//! `mixed` — adds chapters/sections, labels/refs, footnotes, floats, display math, lists, citations.
//! Word counts are calibrated for the `book` class at 11pt with TeX Gyre Pagella (≈ 365 words/page).

use std::fmt::Write as _;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Variant {
    Pure,
    Mixed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum FontSet {
    /// fontspec + TeX Gyre Pagella (OpenType)
    Pagella,
    /// fontspec + Latin Modern (OpenType)
    LatinModern,
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
    let font = match fonts {
        FontSet::Pagella => "\\setmainfont{TeX Gyre Pagella}",
        FontSet::LatinModern => "\\setmainfont{Latin Modern Roman}",
    };
    let mut s = String::new();
    s.push_str("% Generated by `lode gen-book`; do not edit by hand.\n");
    s.push_str("\\documentclass[11pt]{book}\n");
    s.push_str("\\usepackage{fontspec}\n");
    s.push_str(font);
    s.push('\n');
    // unicode-math keeps every font OpenType (no Type1 CM math), which hosts and our rasterizer
    // can render from glyph indices alone.
    s.push_str("\\usepackage{microtype}\n\\usepackage{amsmath}\n\\usepackage{unicode-math}\n");
    s.push_str(match fonts {
        FontSet::Pagella => "\\setmathfont{TeX Gyre Pagella Math}\n",
        FontSet::LatinModern => "\\setmathfont{Latin Modern Math}\n",
    });
    s.push_str("\\usepackage{xcolor}\n");
    if mixed {
        s.push_str("\\usepackage{graphicx}\n\\usepackage[backend=biber,style=numeric]{biblatex}\n\\addbibresource{refs.bib}\n");
    }
    // Deterministic PDF output: suppress all optional info (incl. /ID, dates, producer).
    s.push_str("\\pdfvariable suppressoptionalinfo 1023\\relax\n");
    s.push_str("\\begin{document}\n");
    s
}

pub fn generate(pages: u32, variant: Variant, fonts: FontSet, seed: u64, out: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(out)?;
    let mut rng = Rng::new(seed ^ (pages as u64) << 8 ^ (variant as u64));
    let target_words = pages as usize * 365;
    let mixed = variant == Variant::Mixed;
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
        let sentences = rng.range(2, 9);
        let (mut p, w) = paragraph(&mut rng, sentences, true);
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
                        "\\begin{{figure}}[tbp]\\centering\\includegraphics[width=0.5\\textwidth]{{figure.png}}\\caption{{Figure {} (image).}}\\label{{fig:{}}}\\end{{figure}}\n",
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
        std::fs::write(out.join("figure.png"), png_placeholder())?;
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
