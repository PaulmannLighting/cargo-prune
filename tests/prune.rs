//! Exercise the command against fixture caches and a real Cargo build.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, iter};

// These dependencies are used by the binary exercised by this test target.
use clap as _;
use humantime as _;
use serde_json as _;

const CRATE_CACHE: &str = "example-0000000000000";
const OLD: &str = "s-0000000001-0000000-0000000000000000000000000";
const NEW: &str = "s-0000000002-0000000-0000000000000000000000000";
const WORKING: &str = "s-0000000003-0000000-working";
const WORKING_LOCK: &str = "s-0000000003-0000000.lock";
#[cfg(unix)]
const SESSION_AGE: Duration = Duration::from_secs(6_000);

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// Isolated directory for one test's build output.
struct Fixture(PathBuf);

impl Fixture {
    /// Allocates a private fixture below the system temporary directory.
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("cargo-prune-test-{}-{id}", process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// Creates one session with a small cache payload.
    fn session(&self, profile: &str, name: &str) -> PathBuf {
        let dir = self
            .0
            .join(profile)
            .join("incremental")
            .join(CRATE_CACHE)
            .join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dep-graph.bin"), b"cache").unwrap();
        dir
    }

    /// Runs the standalone binary with an explicit build directory.
    fn prune(&self, args: &[&str]) -> String {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-prune"));
        cmd.arg("--build-dir").arg(&self.0).args(args);
        run(&mut cmd)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn dry_run_and_retention_respect_target_and_profile() {
    let fixture = Fixture::new();
    let old = fixture.session("debug", OLD);
    let newest = fixture.session("debug", NEW);
    let working = fixture.session("debug", WORKING);
    let unknown = fixture.session("debug", "s-unknown-name-format");
    let release = fixture.session("release", OLD);
    let cross = fixture.session("wasm32-wasip1/debug", OLD);
    let artifact = fixture.0.join("debug/executable");
    fs::write(&artifact, b"artifact").unwrap();

    let args = [
        "--older-than=0",
        "--keep-last",
        "1",
        "--target",
        "host",
        "--profile",
        "dev",
    ];
    let mut preview = args.to_vec();
    preview.push("--dry-run");
    assert!(
        fixture
            .prune(&preview)
            .contains("Would remove 2 incremental session(s) (10 bytes")
    );
    assert!(old.exists() && working.exists());

    assert!(
        fixture
            .prune(&args)
            .contains("Removed 2 incremental session(s)")
    );
    assert!(!old.exists() && !working.exists());
    assert!(newest.exists() && unknown.exists() && release.exists() && cross.exists());
    assert!(artifact.exists());
    assert!(
        fixture
            .prune(&args)
            .contains("Removed 0 incremental session(s)")
    );
    fixture.prune(&["--older-than", "0", "--target=wasm32-wasip1"]);
    assert!(!cross.exists() && newest.exists());
}

#[cfg(unix)]
#[test]
fn checks_file_age_and_preserves_symlinked_paths() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let old = fixture.session("debug", OLD);
    let recent = fixture.session("debug", NEW);
    File::open(old.join("dep-graph.bin"))
        .unwrap()
        .set_modified(UNIX_EPOCH)
        .unwrap();
    for dir in [&old, &recent] {
        File::open(dir).unwrap().set_modified(UNIX_EPOCH).unwrap();
    }

    let outside = Fixture::new();
    let outside_session = outside.session("debug", OLD);
    symlink(outside.0.join("debug"), fixture.0.join("linked-profile")).unwrap();
    let linked = fixture.session("debug", WORKING);
    symlink(&outside_session, linked.join("external")).unwrap();

    assert!(
        fixture
            .prune(&[])
            .contains("Removed 1 incremental session(s)")
    );
    assert!(!old.exists() && recent.exists());
    fixture.prune(&["--older-than=0"]);
    assert!(outside_session.join("dep-graph.bin").exists() && linked.exists());
}

#[cfg(unix)]
#[test]
fn prunes_sessions_at_sub_day_thresholds() {
    let fixture = Fixture::new();
    let old = fixture.session("debug", OLD);
    let recent = fixture.session("debug", NEW);
    let modified = SystemTime::now() - SESSION_AGE;
    for path in [&old, &old.join("dep-graph.bin")] {
        File::open(path).unwrap().set_modified(modified).unwrap();
    }

    for (age, eligible) in [
        ("2h", 0),
        ("1d", 0),
        ("1", 0),
        ("1.5h", 1),
        ("90m", 1),
        ("5400s", 1),
        ("1h45m", 0),
    ] {
        assert!(
            fixture
                .prune(&["--older-than", age, "--dry-run"])
                .contains(&format!("Would remove {eligible} incremental session(s)")),
            "{age}"
        );
    }
    assert!(
        fixture
            .prune(&["--older-than", "1h 30m"])
            .contains("Removed 1 incremental session(s)")
    );
    assert!(!old.exists() && recent.exists());
}

#[test]
fn skips_locked_sessions_until_they_are_released() {
    let fixture = Fixture::new();
    let working = fixture.session("debug", WORKING);
    let lock_path = working.with_file_name(WORKING_LOCK);
    let lock = File::create(&lock_path).unwrap();
    lock.try_lock_shared().unwrap();

    let args = ["--older-than=0"];
    assert!(fixture.prune(&args).contains("Skipped 1 locked session(s)"));
    assert!(working.exists() && lock_path.exists());
    drop(lock);

    #[cfg(unix)]
    {
        use rustix::fs::{FlockOperation, fcntl_lock};

        let lock = File::options()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        fcntl_lock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
        assert!(fixture.prune(&args).contains("Skipped 1 locked session(s)"));
        assert!(working.exists() && lock_path.exists());
    }

    assert!(
        fixture
            .prune(&args)
            .contains("Removed 1 incremental session(s)")
    );
    assert!(!working.exists() && !lock_path.exists());
}

#[test]
fn cargo_subcommand_resolves_a_real_build_directory() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = 'example'\nversion = '0.1.0'\nedition = '2024'\n",
    )
    .unwrap();
    fs::write(fixture.0.join("src/main.rs"), "fn main() {}\n").unwrap();
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cargo-prune"))
        .parent()
        .unwrap();
    let process_path = env::var_os("PATH").unwrap_or_default();
    let path =
        env::join_paths(iter::once(bin_dir.to_path_buf()).chain(env::split_paths(&process_path)))
            .unwrap();
    let cargo = || {
        let mut cmd = Command::new(env!("CARGO"));
        cmd.current_dir(&fixture.0)
            .env("PATH", &path)
            .env("CARGO_TARGET_DIR", fixture.0.join("final artifacts"))
            .env(
                "CARGO_BUILD_BUILD_DIR",
                fixture.0.join("intermediate cache"),
            )
            .env("CARGO_INCREMENTAL", "1");
        cmd
    };

    assert!(run(cargo().args(["help", "prune"])).contains("Usage: cargo prune [OPTIONS]"));
    run(cargo().args(["build", "--offline"]));
    let prune_args = [
        "prune",
        "--older-than",
        "0s",
        "--manifest-path",
        "Cargo.toml",
    ];
    assert!(
        run(cargo().args(prune_args).arg("--dry-run"))
            .contains("Would remove 1 incremental session(s)")
    );
    assert!(run(cargo().args(prune_args)).contains("Removed 1 incremental session(s)"));
    assert!(run(cargo().args(prune_args)).contains("Removed 0 incremental session(s)"));
    let executable = format!("example{}", env::consts::EXE_SUFFIX);
    assert!(
        fixture
            .0
            .join("final artifacts/debug")
            .join(executable)
            .is_file()
    );

    // A change to the source forces rustc to recreate a pruned cache.
    fs::write(
        fixture.0.join("src/main.rs"),
        "fn main() { println!(\"rebuilt\"); }\n",
    )
    .unwrap();
    run(cargo().args(["build", "--offline"]));
    assert!(
        run(cargo().args(prune_args).arg("--dry-run"))
            .contains("Would remove 1 incremental session(s)")
    );
}

#[test]
fn invalid_options_fail_before_pruning() {
    let fixture = Fixture::new();
    let session = fixture.session("debug", OLD);
    for invalid in [
        "--older-than=18446744073709551615",
        "--older-than=18446744073709551615h",
        "--older-than=1fortnight",
        "--older-than=1.5",
        "--target=../debug",
        "--profile=",
        "--build-dir=",
        "--dry-run=false",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-prune"))
            .arg("--target-dir")
            .arg(&fixture.0)
            .args(["--older-than=0", invalid])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{invalid}");
        assert!(session.is_dir(), "{invalid}");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-prune"));
    cmd.arg("--target-dir")
        .arg(&fixture.0)
        .args(["--older-than=0", "-n"]);
    assert!(run(&mut cmd).contains("Would remove 1 incremental session(s)"));
}

#[test]
fn compound_duration_overflow_is_a_cli_error() {
    let fixture = Fixture::new();
    let session = fixture.session("debug", OLD);
    let max_seconds = Duration::MAX.as_secs();
    let max_nanoseconds = Duration::MAX.subsec_nanos();
    let max_age = format!("{max_seconds}s {max_nanoseconds}ns");
    assert!(
        fixture
            .prune(&["--older-than", &max_age, "--dry-run"])
            .contains("Would remove 0 incremental session(s)")
    );

    let overflow = format!("{max_age} 1ns");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-prune"))
        .arg("--build-dir")
        .arg(&fixture.0)
        .args(["--older-than", &overflow])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("number is too large"), "{stderr}");
    assert!(session.is_dir());
}

/// Runs a command and returns stdout, including stderr in any failure.
fn run(cmd: &mut Command) -> String {
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
