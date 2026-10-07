mod gen_book;
mod slice;

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
        Cmd::GenBook { pages, variant, fonts, seed, out } => {
            gen_book::generate(pages, variant, fonts, seed, &out)?;
            println!("wrote {}", out.display());
        }
    }
    Ok(())
}
