//! Arguments accepted by both `cargo prune` and the standalone binary.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use std::{env, mem};

use clap::Parser;
use clap::builder::{OsStringValueParser, TypedValueParser};

const DEFAULT_AGE: &str = "30d";
const DEFAULT_KEEP_LAST: usize = 0;

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

    /// Minimum age, e.g. 1d, 2h, or "1h 30m" (bare integers mean days).
    #[arg(long = "older-than", value_name = "DURATION", default_value = DEFAULT_AGE, value_parser = age)]
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

/// Parses a human-readable duration, treating a bare integer as days.
fn age(value: &str) -> Result<Duration, humantime::DurationError> {
    value.parse::<u64>().map_or_else(
        |_| checked_duration(value),
        |days| humantime::parse_duration(&format!("{days}d")),
    )
}

/// Checks compound totals before humantime can panic while carrying nanoseconds into seconds.
fn checked_duration(value: &str) -> Result<Duration, humantime::DurationError> {
    let mut has_unit = false;
    let boundaries = value.match_indices(|character: char| {
        if character.is_ascii_digit() {
            return mem::take(&mut has_unit);
        }
        has_unit |= character.is_ascii_alphabetic() || character == 'µ';
        false
    });

    let mut start = 0;
    let mut total = Duration::ZERO;
    for end in boundaries.map(|(end, _)| end).chain([value.len()]) {
        if start == 0 && end == value.len() {
            break;
        }
        let span = value
            .get(start..end)
            .and_then(|span| humantime::parse_duration(span).ok());
        let Some(span) = span else {
            // The whole input preserves humantime's error positions for malformed spans.
            break;
        };
        total = total
            .checked_add(span)
            .ok_or(humantime::DurationError::NumberOverflow)?;
        start = end;
    }
    humantime::parse_duration(value)
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
