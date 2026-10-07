mod gen_book;

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
        Cmd::GenBook { pages, variant, fonts, seed, out } => {
            gen_book::generate(pages, variant, fonts, seed, &out)?;
            println!("wrote {}", out.display());
        }
    }
    Ok(())
}
