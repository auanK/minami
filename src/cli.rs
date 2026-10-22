use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use minami::{extract_epub, extract_epub_batched, inspect_epub, validate_epub};

#[derive(Parser)]
#[command(
    name = "minami",
    version,
    about = "Inspect EPUB 2 structure, extract translation input, and validate translated responses"
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
    /// Validate translated X 1.0 responses against the original EPUB.
    Validate {
        file: PathBuf,
        /// Regenerated control map to compare with the selected EPUB.
        #[arg(long, value_name = "FILE")]
        control: PathBuf,
        /// Translation response; repeat for multiple complete or partial files.
        #[arg(long = "translation", value_name = "FILE", required = true)]
        translations: Vec<PathBuf>,
        /// Validate only the segments selected by this X envelope.
        #[arg(long, value_name = "FILE")]
        expected_batch: Option<PathBuf>,
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
        Command::Validate {
            file,
            control,
            translations,
            expected_batch,
        } => {
            return match validate_epub(&file, &control, &translations, expected_batch.as_deref()) {
                Ok(summary) => {
                    let scope = if summary.complete {
                        "complete extraction"
                    } else {
                        "selected batch; not a complete translation"
                    };
                    let mut stdout = io::stdout().lock();
                    if let Err(error) = writeln!(
                        stdout,
                        "validated {} segments ({scope})",
                        summary.validated_segments
                    ) {
                        eprintln!("minami: writing validation result: {error}");
                        return ExitCode::FAILURE;
                    }
                    ExitCode::SUCCESS
                }
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
