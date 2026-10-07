use lode_core::fixtures as gen_book;
mod bench;
mod probe;
mod serve;
mod slice;
mod verify;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "lode", version, about = "Real-time LuaTeX compilation library driver")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a deterministic fixture book.
    /// M1 vertical slice: capture, persistent server, display list, PDF check, timing.
    Slice {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value = "main.tex")]
        main: String,
        /// Index among eligible top-level paragraphs (default: the middle one).
        #[arg(long)]
        paragraph: Option<usize>,
        #[arg(long, default_value_t = 200)]
        edits: usize,
        #[arg(long, default_value = "build/slice")]
        build: PathBuf,
        #[arg(long)]
        json_out: Option<PathBuf>,
    },
    /// Run the fidelity layers over a whole project (all eligible paragraphs, all pages).
    Verify {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value = "main.tex")]
        main: String,
        #[arg(long, default_value = "build/verify")]
        build: PathBuf,
        #[arg(long, default_value_t = 150)]
        dpi: u32,
        /// Also run the rendered comparison (needs python3 + PyMuPDF).
        #[arg(long)]
        raster: bool,
        #[arg(long)]
        json_out: Option<PathBuf>,
        #[arg(long)]
        max_paragraphs: Option<usize>,
    },
    /// JSON-lines session front end (commands on stdin, events on stdout).
    Serve {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value = "main.tex")]
        main: String,
        #[arg(long)]
        build: Option<PathBuf>,
    },
    /// Open a session, apply one edit, print the resulting paragraph update.
    Edit {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value = "main.tex")]
        main: String,
        #[arg(long)]
        byte: Option<usize>,
        /// Insert after the first occurrence of this text.
        #[arg(long)]
        find: Option<String>,
        #[arg(long, default_value = " edited")]
        text: String,
        #[arg(long, default_value_t = 60)]
        wait: u64,
    },
    /// Compare two PDFs for typesetting equality (content streams, fonts, images; metadata ignored).
    PdfCompare { a: PathBuf, b: PathBuf },
    /// Convert a binary display list to its JSON mirror.
    Dl2json { file: PathBuf },
    /// Benchmarks: in-engine line breaking (hardware factor) and warm round trips with gates.
    Bench {
        /// Projects to benchmark (fixture directories with main.tex).
        #[arg(long, num_args = 1..)]
        project: Vec<PathBuf>,
        #[arg(long, default_value = "short,medium,long,inline-math", value_delimiter = ',')]
        categories: Vec<String>,
        #[arg(long, default_value_t = 300)]
        samples: usize,
        #[arg(long, default_value_t = 100)]
        inner: usize,
        #[arg(long, default_value = "build/bench")]
        build: PathBuf,
        #[arg(long, default_value = "bench/results")]
        out: PathBuf,
        /// Fewer samples everywhere (smoke run).
        #[arg(long)]
        quick: bool,
    },
    /// Export a PDF through a session (clean build loop, status reported) and optionally compare
    /// it with an independent clean lualatex build.
    Export {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value = "main.tex")]
        main: String,
        #[arg(long)]
        out: PathBuf,
        /// Also build the project independently and compare with `lode pdf-compare` rules.
        #[arg(long)]
        check: bool,
    },
    /// Latency breakdown of the fast path (direct server, engine profile, session) on one project.
    Probe {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value = "main.tex")]
        main: String,
        #[arg(long, default_value_t = 200)]
        n: usize,
        #[arg(long, default_value = "build/probe")]
        build: PathBuf,
    },
    GenBook {
        #[arg(long, default_value_t = 10)]
        pages: u32,
        #[arg(long, value_enum, default_value_t = gen_book::Variant::Pure)]
        variant: gen_book::Variant,
        #[arg(long, value_enum, default_value_t = gen_book::FontSet::Pagella)]
        fonts: gen_book::FontSet,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Slice { project, main, paragraph, edits, build, json_out } => {
            slice::run(slice::SliceOpts { project, main, paragraph, edits, build, json_out })?;
        }
        Cmd::Verify { project, main, build, dpi, raster, json_out, max_paragraphs } => {
            let r = verify::run(verify::VerifyOpts { project, main, build, dpi, raster, json_out, max_paragraphs })?;
            if !(r.layer1_pass && r.layer2_pass && r.layer3_pass.unwrap_or(true) && r.capture_pdf_equals_clean) {
                std::process::exit(1);
            }
        }
        Cmd::Serve { project, main, build } => serve::run(project, main, build)?,
        Cmd::Edit { project, main, byte, find, text, wait } => serve::edit_once(project, main, byte, find, text, wait)?,
        Cmd::PdfCompare { a, b } => {
            let d = lode_verify::pdfcompare::compare(&a, &b)?;
            println!("{}", serde_json::to_string_pretty(&d)?);
            if !d.equal {
                std::process::exit(1);
            }
        }
        Cmd::Dl2json { file } => {
            let bytes = std::fs::read(&file)?;
            let dl = lode_dl::DisplayList::from_binary(&bytes)?;
            println!("{}", serde_json::to_string_pretty(&dl)?);
        }
        Cmd::Bench { project, categories, samples, inner, build, out, quick } => {
            let (samples, inner) = if quick { (30, 10) } else { (samples, inner) };
            let r = bench::run_all(project, categories, samples, inner, build, out, quick)?;
            if !r.pass {
                std::process::exit(1);
            }
        }
        Cmd::Export { project, main, out, check } => {
            let session = lode_core::Session::open(lode_core::SessionConfig::new(&project, main.clone()))?;
            let job = session.export_pdf(&out);
            let (ev, _) = session.wait_for(std::time::Duration::from_secs(1800), |e| matches!(e, lode_core::Event::PdfExported { job_id, .. } if *job_id == job));
            let Some(lode_core::Event::PdfExported { path, status, converged, passes, .. }) = ev else { anyhow::bail!("no export result") };
            println!("export: status {:?} converged {} passes {} path {:?}", status, converged, passes, path);
            session.close();
            if check {
                let tl = lode_core::texlive::TexLive::discover()?;
                let tmp = std::env::temp_dir().join(format!("lode-export-check-{}", std::process::id()));
                let o = lode_core::background::run_pass(&tl, &project.canonicalize()?, &main, &tmp, 5, lode_core::background::BibTool::Auto, false)?;
                let d = lode_verify::pdfcompare::compare(&out, &o.capture.pdf)?;
                println!("independent clean build: {} passes, equal = {} {:?}", o.passes, d.equal, d.differences.iter().take(3).collect::<Vec<_>>());
                let _ = std::fs::remove_dir_all(&tmp);
                if !d.equal || !converged {
                    std::process::exit(1);
                }
            }
        }
        Cmd::Probe { project, main, n, build } => probe::run(project, main, n, build)?,
        Cmd::GenBook { pages, variant, fonts, seed, out } => {
            gen_book::generate(pages, variant, fonts, seed, &out)?;
            println!("wrote {}", out.display());
        }
    }
    Ok(())
}
