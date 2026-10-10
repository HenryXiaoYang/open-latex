use rtex_core::fixtures as gen_book;
mod bench;
mod probe;
mod serve;
mod slice;
mod verify;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rtex",
    version,
    about = "Real-time LuaTeX compilation library driver"
)]
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
        /// Only these unit ids in layer 1 (comma separated).
        #[arg(long, value_delimiter = ',')]
        units: Vec<i64>,
        /// Print fast and capture rows of differing units.
        #[arg(long)]
        dump_rows: bool,
        /// Fail unless at least this many units are eligible for the fast path (regression gate).
        #[arg(long)]
        min_eligible: Option<usize>,
        /// Research: ignore the allow-list and let the row comparison judge every unit.
        #[arg(long)]
        permissive: bool,
        /// Pass layer 1 with up to this many differing units (fixtures with known state-dependent
        /// units, which the comparison must catch).
        #[arg(long)]
        max_differing: Option<usize>,
        /// Pass layer 1 only with exactly this many differing units (the known state-dependent
        /// units must be caught, neither more nor fewer).
        #[arg(long)]
        expect_differing: Option<usize>,
        /// Picture cache check: a second capture pass takes the pictures from the first pass's
        /// PDF; units, placements (and with --raster the rendered pages) must be identical.
        #[arg(long)]
        pic_cache: bool,
        /// With --pic-cache: fail unless at least this many pictures were recorded and reused.
        #[arg(long)]
        min_cached: Option<usize>,
    },
    /// JSON-lines session front end (commands on stdin, events on stdout).
    Serve {
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value = "main.tex")]
        main: String,
        #[arg(long)]
        build: Option<PathBuf>,
        /// Per-unit fast-path budget in milliseconds: a unit whose compiles take longer three
        /// times in a row goes to the background path until the next layout (default 50).
        #[arg(long, default_value_t = 50)]
        fast_budget_ms: u64,
        /// How units qualify for the fast path: probe (default) or allowlist.
        #[arg(long, default_value = "probe")]
        eligibility: String,
        /// Draw every picture environment on every background pass instead of reusing the
        /// unchanged ones from an earlier pass's PDF.
        #[arg(long)]
        no_picture_cache: bool,
        /// Write diagnostics here: a bundle per engine failure (request source, context,
        /// server log and stage trace) and one line per live compile in requests.log
        /// (default: $RTEX_DEBUG_DIR when set).
        #[arg(long)]
        debug_dir: Option<PathBuf>,
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
        #[arg(
            long,
            default_value = "short,medium,long,inline-math",
            value_delimiter = ','
        )]
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
        /// Also build the project independently and compare with `rtex pdf-compare` rules.
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
        Cmd::Slice {
            project,
            main,
            paragraph,
            edits,
            build,
            json_out,
        } => {
            slice::run(slice::SliceOpts {
                project,
                main,
                paragraph,
                edits,
                build,
                json_out,
            })?;
        }
        Cmd::Verify {
            project,
            main,
            build,
            dpi,
            raster,
            json_out,
            max_paragraphs,
            units,
            dump_rows,
            min_eligible,
            permissive,
            max_differing,
            expect_differing,
            pic_cache,
            min_cached,
        } => {
            let r = verify::run(verify::VerifyOpts {
                project,
                main,
                build,
                dpi,
                raster,
                json_out,
                max_paragraphs,
                units,
                dump_rows,
                min_eligible,
                permissive,
                max_differing,
                expect_differing,
                pic_cache,
                min_cached,
            })?;
            if !(r.layer1_pass
                && r.layer2_pass
                && r.layer3_pass.unwrap_or(true)
                && r.capture_pdf_equals_clean
                && r.pic_cache.as_ref().map(|p| p.pass).unwrap_or(true))
            {
                std::process::exit(1);
            }
        }
        Cmd::Serve {
            project,
            main,
            build,
            fast_budget_ms,
            eligibility,
            no_picture_cache,
            debug_dir,
        } => serve::run(
            project,
            main,
            build,
            fast_budget_ms,
            &eligibility,
            !no_picture_cache,
            debug_dir,
        )?,
        Cmd::Edit {
            project,
            main,
            byte,
            find,
            text,
            wait,
        } => serve::edit_once(project, main, byte, find, text, wait)?,
        Cmd::PdfCompare { a, b } => {
            let d = rtex_verify::pdfcompare::compare(&a, &b)?;
            println!("{}", serde_json::to_string_pretty(&d)?);
            if !d.equal {
                std::process::exit(1);
            }
        }
        Cmd::Dl2json { file } => {
            let bytes = std::fs::read(&file)?;
            let dl = rtex_dl::DisplayList::from_binary(&bytes)?;
            println!("{}", serde_json::to_string_pretty(&dl)?);
        }
        Cmd::Bench {
            project,
            categories,
            samples,
            inner,
            build,
            out,
            quick,
        } => {
            let (samples, inner) = if quick { (30, 10) } else { (samples, inner) };
            let r = bench::run_all(project, categories, samples, inner, build, out, quick)?;
            if !r.pass {
                std::process::exit(1);
            }
        }
        Cmd::Export {
            project,
            main,
            out,
            check,
        } => {
            let session =
                rtex_core::Session::open(rtex_core::SessionConfig::new(&project, main.clone()))?;
            let job = session.export_pdf(&out);
            let (ev, _) = session.wait_for(
                std::time::Duration::from_secs(1800),
                |e| matches!(e, rtex_core::Event::PdfExported { job_id, .. } if *job_id == job),
            );
            let Some(rtex_core::Event::PdfExported {
                path,
                status,
                converged,
                passes,
                ..
            }) = ev
            else {
                anyhow::bail!("no export result")
            };
            println!(
                "export: status {:?} converged {} passes {} path {:?}",
                status, converged, passes, path
            );
            session.close();
            if check {
                let tl = rtex_core::texlive::TexLive::discover()?;
                let tmp =
                    std::env::temp_dir().join(format!("rtex-export-check-{}", std::process::id()));
                let o = rtex_core::background::run_pass(
                    &tl,
                    &rtex_core::paths::canonical(&project)?,
                    &main,
                    &tmp,
                    5,
                    rtex_core::background::BibTool::Auto,
                    false,
                )?;
                let d = rtex_verify::pdfcompare::compare(&out, &o.capture.pdf)?;
                println!(
                    "independent clean build: {} passes, equal = {} {:?}",
                    o.passes,
                    d.equal,
                    d.differences.iter().take(3).collect::<Vec<_>>()
                );
                let _ = std::fs::remove_dir_all(&tmp);
                if !d.equal || !converged {
                    std::process::exit(1);
                }
            }
        }
        Cmd::Probe {
            project,
            main,
            n,
            build,
        } => probe::run(project, main, n, build)?,
        Cmd::GenBook {
            pages,
            variant,
            fonts,
            seed,
            out,
        } => {
            gen_book::generate(pages, variant, fonts, seed, &out)?;
            println!("wrote {}", out.display());
        }
    }
    Ok(())
}
