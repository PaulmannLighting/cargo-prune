# cargo-prune

`cargo prune` frees disk space by removing stale Rust incremental compilation sessions. It works on the current workspace and keeps compiled binaries, dependency artifacts, build-script output, and Cargo's download cache available.

## Install

From this directory, using Rust 1.89 or later:

```sh
cargo install --path . --locked
```

## Use

Run the command from a Cargo package or workspace:

```sh
cargo prune --dry-run
cargo prune
cargo prune --older-than 14 --keep-last 1 --profile dev --target host
```

By default, sessions with no files or directories modified in the last 30 days are eligible. `--older-than 0` makes any unlocked session eligible by age. `--keep-last N` protects the `N` most recent completed sessions for each crate cache, even when they exceed the age threshold. This is useful if you want to keep a warm incremental cache for each crate. A later build can regenerate pruned data as needed.

| Option | Effect |
| --- | --- |
| `-n`, `--dry-run` | List eligible sessions and their file sizes without removing them. |
| `--older-than DAYS` | Set the minimum age in whole days. The default is 30. |
| `--keep-last COUNT` | Preserve up to this many completed sessions per crate cache. The default is 0. |
| `--profile NAME` | Limit pruning to one profile. `dev` and `test` select `debug`, and `bench` selects `release`. |
| `--target NAME` | Select a target directory name, or `host` for builds that omit Cargo's `--target`. |
| `--manifest-path PATH` | Resolve the workspace using a specific `Cargo.toml`. |
| `--build-dir PATH`, `--target-dir PATH` | Scan a specific build directory, including outside a Cargo workspace. |
| `-v`, `--verbose` | List each removed session. |

Cargo metadata resolves configured build and target directories. Older Cargo versions that report only the target directory use that field. Use `--build-dir` when pruning a separate build directory an older Cargo does not report, or when cleaning an old build directory after changing Cargo configuration. Both host and cross-target caches are included by default.

The summary reports the logical byte sizes of removed cache files. Actual disk space recovered can differ, for example when another file still hard-links to the same data. Sessions locked by a running compiler are skipped.
