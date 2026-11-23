//! Arguments accepted by both `cargo prune` and the standalone binary.

use std::env;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use clap::builder::{OsStringValueParser, TypedValueParser};

const DEFAULT_DAYS: &str = "30";
const DEFAULT_KEEP_LAST: usize = 0;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Prune stale Rust incremental compilation caches.
#[derive(Parser)]
#[command(
    bin_name = "cargo prune",
    version,
    no_binary_name = true,
    args_override_self = true
)]
pub struct Options {
    /// Show what would be removed.
    #[arg(short = 'n', long)]
    pub(crate) dry_run: bool,

    /// Minimum age in whole days.
    #[arg(long = "older-than", value_name = "DAYS", default_value = DEFAULT_DAYS, value_parser = age)]
    pub(crate) age: Duration,

    /// Keep this many newest completed sessions per crate.
    #[arg(long, value_name = "COUNT", default_value_t = DEFAULT_KEEP_LAST)]
    pub(crate) keep_last: usize,

    /// Select a Cargo profile (default: all).
    #[arg(long, value_name = "NAME", value_parser = OsStringValueParser::new().try_map(profile))]
    pub(crate) profile: Option<OsString>,

    /// Select a target directory name, or 'host' (default: all).
    #[arg(long, value_name = "NAME", value_parser = OsStringValueParser::new().try_map(component))]
    pub(crate) target: Option<OsString>,

    /// Path to the workspace or package Cargo.toml.
    #[arg(long, value_name = "PATH")]
    pub(crate) manifest_path: Option<PathBuf>,

    /// Scan this build directory directly.
    #[arg(long, value_name = "PATH", visible_alias = "target-dir")]
    pub(crate) build_dir: Option<PathBuf>,

    /// List each removed session.
    #[arg(short, long)]
    pub(crate) verbose: bool,
}

impl Options {
    /// Parses process arguments after consuming Cargo's optional subcommand prefix.
    pub(crate) fn parse() -> Self {
        let mut args = env::args_os().skip(1).peekable();
        if args.peek().is_some_and(|arg| arg == "prune") {
            args.next();
        }
        Self::parse_from(args)
    }
}

/// Parses a whole number of days and checks that the result fits in a duration.
fn age(value: &str) -> Result<Duration, &'static str> {
    let days: u64 = value
        .parse()
        .map_err(|_| "expected a nonnegative integer")?;
    days.checked_mul(SECONDS_PER_DAY)
        .map(Duration::from_secs)
        .ok_or("number of days is too large")
}

/// Resolves built-in profile aliases to their output directory names.
fn profile(value: OsString) -> Result<OsString, &'static str> {
    let name = component(value)?;
    Ok(match name.to_str() {
        Some("dev" | "test") => "debug".into(),
        Some("bench") => "release".into(),
        _ => name,
    })
}

/// Validates that a profile or target name is a single directory component.
fn component(value: OsString) -> Result<OsString, &'static str> {
    if Path::new(&value)
        .components()
        .eq([Component::Normal(&value)])
    {
        Ok(value)
    } else {
        Err("expected a directory name")
    }
}
