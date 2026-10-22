use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use minami::{extract_epub, extract_epub_batched, inspect_epub};

#[derive(Parser)]
#[command(
    name = "minami",
    version,
    about = "Inspect EPUB 2 structure and extract external translation input"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect the package, reading order and NCX navigation as JSON.
    Inspect { file: PathBuf },
    /// Extract translation input and reconstruction metadata to a new directory.
    Extract {
        file: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Split source segments into section-scoped JSON batches of at most this many bytes.
        #[arg(long, value_name = "N")]
        batch_max_bytes: Option<usize>,
    },
}

pub fn run() -> ExitCode {
    let args = Args::parse();
    let result = match args.command {
        Command::Inspect { file } => inspect_epub(&file),
        Command::Extract {
            file,
            output,
            batch_max_bytes,
        } => {
            let result = match batch_max_bytes {
                Some(limit) => extract_epub_batched(&file, &output, limit),
                None => extract_epub(&file, &output),
            };
            return match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("minami: {error}");
                    ExitCode::FAILURE
                }
            };
        }
    };
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            eprintln!("minami: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut json = match serde_json::to_vec_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("minami: serializing inspection: {error}");
            return ExitCode::FAILURE;
        }
    };
    json.push(b'\n');
    if let Err(error) = io::stdout().lock().write_all(&json) {
        eprintln!("minami: writing JSON: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
