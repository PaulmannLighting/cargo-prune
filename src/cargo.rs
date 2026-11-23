//! Resolve Cargo's build directory without resolving or downloading dependencies.

use std::env;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::Result;
use crate::cli::Options;

/// Uses an explicit override or reads the intermediate build path from Cargo metadata.
pub fn build_dir(options: &Options) -> Result<PathBuf> {
    if let Some(path) = &options.build_dir {
        return Ok(path.clone());
    }

    let mut cargo = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo.args([
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--offline",
    ]);
    if let Some(manifest) = &options.manifest_path {
        cargo.arg("--manifest-path").arg(manifest);
    }

    let output = cargo
        .output()
        .map_err(|error| format!("could not run Cargo: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        )
        .into());
    }

    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("could not parse Cargo metadata: {error}"))?;
    let path = metadata["build_directory"]
        .as_str()
        .or_else(|| metadata["target_directory"].as_str())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("Cargo metadata did not report an absolute build directory")?;
    Ok(path)
}
