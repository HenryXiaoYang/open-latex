use lode_core::fixtures as gen_book;
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
        Cmd::GenBook { pages, variant, fonts, seed, out } => {
            gen_book::generate(pages, variant, fonts, seed, &out)?;
            println!("wrote {}", out.display());
        }
    }
    Ok(())
}
