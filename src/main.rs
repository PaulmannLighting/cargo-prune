//! Cargo subcommand for pruning stale incremental compilation sessions.

use std::process::ExitCode;
use std::time::SystemTime;

use bytesize::ByteSize;
use cli::Options;

mod cache;
mod cargo;
mod cli;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    match run(&Options::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Finds the selected cache and reports the effect of pruning it.
fn run(options: &Options) -> Result<()> {
    let root = cargo::build_dir(options)?;
    let report = cache::prune(&root, options, SystemTime::now())?;
    let action = if options.dry_run {
        "Would remove"
    } else {
        "Removed"
    };
    let size = ByteSize::b(report.bytes);
    println!(
        "{action} {} incremental session(s) ({size} of cache files)",
        report.sessions,
    );
    if report.busy > 0 {
        println!("Skipped {} locked session(s)", report.busy);
    }
    Ok(())
}
