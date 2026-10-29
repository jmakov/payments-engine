//! Command-line entry point: `payments-engine transactions.csv > accounts.csv`.

use std::fs;
use std::io;
use std::path;

use clap::Parser;
use eyre::WrapErr;

/// Input path accepted by the command-line application.
#[derive(clap::Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Path to the transaction CSV file.
    #[arg(value_name = "FILE_PATH")]
    file_path: path::PathBuf,
}

/// Opens the input and runs the reader and engine workers.
fn run(path: &path::Path) -> eyre::Result<()> {
    let input =
        fs::File::open(path).wrap_err_with(|| format!("Failed to open {}", path.display()))?;
    payments_engine::run(input, io::stdout())
}

/// Runs the engine on the file given on the command line.
fn main() -> eyre::Result<()> {
    let args = Args::parse();
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env_lossy();
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(false)
        .with_thread_names(true)
        .init();

    run(&args.file_path).inspect_err(|error| {
        tracing::error!(%error, "processing failed");
    })
}
