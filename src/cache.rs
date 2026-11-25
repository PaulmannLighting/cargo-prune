//! Discover and prune rustc sessions under Cargo's incremental cache directories.

use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bytesize::ByteSize;
use indicatif::{ProgressBar, ProgressStyle};
#[cfg(unix)]
use rustix::fs::{FlockOperation, fcntl_lock};
#[cfg(unix)]
use rustix::io::Errno;

use crate::cli::Options;
use crate::progress::{CHECKING, ProgressBarExt};

const INCREMENTAL: &str = "incremental";
const HOST: &str = "host";
const WORKING: &str = "working";
const CRATE_HASH_LEN: usize = 13;
const TIMESTAMP_LEN: usize = 10;
const NONCE_LEN: usize = 7;
const SESSION_HASH_LEN: usize = 25;
const BASE_SESSION_WORK: u64 = 1;
const BYTES_PER_WORK_UNIT: u64 = 1024 * 1024;

/// Counters for the current prune operation.
#[derive(Default)]
pub struct Report {
    pub(crate) sessions: usize,
    pub(crate) bytes: u64,
    pub(crate) busy: usize,
}

impl Report {
    /// Formats the running session count and cache size for the progress bar.
    fn progress_message(&self, dry_run: bool, remaining_bytes: u64) -> String {
        let size = ByteSize::b(self.bytes);
        if dry_run {
            format!("{} eligible ({size})", self.sessions)
        } else {
            let total = ByteSize::b(self.bytes.saturating_add(remaining_bytes));
            format!("{} removed ({size}/{total})", self.sessions)
        }
    }
}

/// A recognized rustc session and the lock shared by its working and completed names.
struct Session {
    path: PathBuf,
    lock_path: PathBuf,
    completed: bool,
    modified: SystemTime,
}

/// A discovered session with its retention decision and sizing result.
struct PlannedSession {
    session: Session,
    retained: bool,
    estimate: SessionEstimate,
}

/// Estimated processing work and logical cache bytes expected to be removed.
#[derive(Default)]
struct SessionEstimate {
    work: u64,
    bytes: u64,
}

/// Eligibility of the lock file at the time of the operation.
enum SessionLock {
    Held { _file: File },

    Missing,

    Busy,

    Unknown,
}

/// File bytes, filesystem entries, and most recent modification time in a session.
struct SessionStats {
    bytes: u64,
    entries: u64,
    modified: SystemTime,
}

/// Discovers the selected sessions, then tracks completed sessions and estimated work.
pub fn prune(
    root: &Path,
    options: &Options,
    now: SystemTime,
    progress: &ProgressBar,
    process_style: ProgressStyle,
    work_progress: &ProgressBar,
) -> io::Result<Report> {
    let interactive = progress.start_scan();
    let mut report = Report::default();
    let Some(root) = optional(fs::canonicalize(root))? else {
        return Ok(report);
    };

    let estimate_sizes = interactive && !options.dry_run;
    let mut plan = Vec::new();
    for dir in directories(&root)? {
        if options
            .target
            .as_deref()
            .is_none_or(|target| target == HOST)
        {
            scan_profile(&dir, options, now, estimate_sizes, &mut plan)?;
        }
        if options
            .target
            .as_deref()
            .is_none_or(|target| target != HOST && dir.file_name() == Some(target))
        {
            for profile in directories(&dir)? {
                scan_profile(&profile, options, now, estimate_sizes, &mut plan)?;
            }
        }
    }

    if plan.is_empty() {
        return Ok(report);
    }
    let mut remaining_work = plan
        .iter()
        .map(|item| item.estimate.work)
        .fold(0, u64::saturating_add);
    let mut remaining_bytes = plan
        .iter()
        .map(|item| item.estimate.bytes)
        .fold(0, u64::saturating_add);
    work_progress.start_estimate(remaining_work);
    progress.start_processing(
        plan.len(),
        report.progress_message(options.dry_run, remaining_bytes),
        process_style,
    );

    for item in plan {
        let previous_count = report.sessions;
        let actual_work = if item.retained {
            0
        } else {
            item.session.prune(options, now, &mut report, progress)?
        };
        if interactive {
            let done = if estimate_sizes {
                actual_work
            } else {
                BASE_SESSION_WORK
            };
            remaining_work = remaining_work.saturating_sub(item.estimate.work);
            remaining_bytes = remaining_bytes.saturating_sub(item.estimate.bytes);
            work_progress.record_work(done, remaining_work);
        }
        let message = (report.sessions != previous_count || item.estimate.bytes > 0)
            .then(|| report.progress_message(options.dry_run, remaining_bytes));
        progress.record_session(message);
    }
    progress.force_draw();
    Ok(report)
}

/// Discovers sessions and applies retention within each crate, newest first.
fn scan_profile(
    profile: &Path,
    options: &Options,
    now: SystemTime,
    estimate_sizes: bool,
    plan: &mut Vec<PlannedSession>,
) -> io::Result<()> {
    if options
        .profile
        .as_deref()
        .is_some_and(|name| profile.file_name() != Some(name))
    {
        return Ok(());
    }
    let incremental = profile.join(INCREMENTAL);
    if !metadata(&incremental)?.is_some_and(|info| info.is_dir()) {
        return Ok(());
    }

    for crate_dir in directories(&incremental)? {
        if !crate_dir
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.rsplit_once('-'))
            .is_some_and(|(name, hash)| !name.is_empty() && is_base36(hash, CRATE_HASH_LEN))
        {
            continue;
        }
        let mut sessions = Vec::new();
        for dir in directories(&crate_dir)? {
            if let Some(session) = Session::read(dir)? {
                sessions.push(session);
            }
        }
        // rustc encodes the timestamp at a fixed width, so it sorts chronologically.
        sessions.sort_unstable_by(|left, right| right.path.cmp(&left.path));
        let mut keep = options.keep_last;
        for session in sessions {
            let retained = session.completed && keep > 0;
            if retained {
                keep -= 1;
            }
            let estimate = if !estimate_sizes {
                SessionEstimate {
                    work: BASE_SESSION_WORK,
                    ..SessionEstimate::default()
                }
            } else if retained {
                SessionEstimate::default()
            } else {
                session.estimate(now, options)
            };
            plan.push(PlannedSession {
                session,
                retained,
                estimate,
            });
        }
    }
    Ok(())
}

impl Session {
    /// Recognizes `s-<timestamp>-<nonce>-<hash|working>` session directories.
    fn read(path: PathBuf) -> io::Result<Option<Self>> {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let mut parts = name.split('-');
        let (Some("s"), Some(timestamp), Some(nonce), Some(state), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Ok(None);
        };
        let completed = state != WORKING;
        if !is_base36(timestamp, TIMESTAMP_LEN)
            || !is_base36(nonce, NONCE_LEN)
            || (completed && !is_base36(state, SESSION_HASH_LEN))
        {
            return Ok(None);
        }
        let Some(info) = metadata(&path)?.filter(Metadata::is_dir) else {
            return Ok(None);
        };
        let lock_path = path.with_file_name(format!("s-{timestamp}-{nonce}.lock"));
        let modified = at(&path, info.modified())?;
        Ok(Some(Self {
            path,
            lock_path,
            completed,
            modified,
        }))
    }

    /// Sizes a potential removal while holding its lock, without changing the cache.
    fn estimate(&self, now: SystemTime, options: &Options) -> SessionEstimate {
        if !is_old(self.modified, now, options) {
            return SessionEstimate::default();
        }
        let Ok(_lock @ (SessionLock::Held { .. } | SessionLock::Missing)) =
            SessionLock::try_acquire(&self.lock_path)
        else {
            return SessionEstimate::default();
        };
        // Sizing is advisory. Processing rechecks the session and reports current I/O errors.
        SessionStats::read(&self.path).ok().flatten().map_or_else(
            SessionEstimate::default,
            |stats| {
                let removing = is_old(stats.modified, now, options);
                SessionEstimate {
                    work: stats.work(removing),
                    bytes: if removing { stats.bytes } else { 0 },
                }
            },
        )
    }

    /// Checks the age, removes the session under its lock, and returns the work performed.
    fn prune(
        &self,
        options: &Options,
        now: SystemTime,
        report: &mut Report,
        progress: &ProgressBar,
    ) -> io::Result<u64> {
        if !is_old(self.modified, now, options) {
            return Ok(0);
        }
        let lock = SessionLock::try_acquire(&self.lock_path)?;
        match lock {
            SessionLock::Busy => {
                report.busy += 1;
                return Ok(0);
            }
            SessionLock::Unknown => return Ok(0),
            _ => {}
        }
        let Some(stats) = SessionStats::read(&self.path)? else {
            return Ok(0);
        };
        if !is_old(stats.modified, now, options) {
            return Ok(stats.work(false));
        }

        let size = ByteSize::b(stats.bytes);
        if !options.dry_run {
            progress.set_prefix(format!("Removing ({size})"));
            let removed = optional(at(&self.path, fs::remove_dir_all(&self.path)))?;
            progress.set_prefix(CHECKING);
            if removed.is_none() {
                return Ok(stats.work(false));
            }
            if let SessionLock::Held { .. } = &lock {
                optional(at(&self.lock_path, fs::remove_file(&self.lock_path)))?;
            }
        }
        if options.dry_run || options.verbose {
            let action = if options.dry_run {
                "Would remove"
            } else {
                "Removed"
            };
            progress.suspend(|| println!("{action} {} ({size})", self.path.display()));
        }
        report.sessions += 1;
        report.bytes = report.bytes.saturating_add(stats.bytes);
        Ok(stats.work(!options.dry_run))
    }
}

impl SessionLock {
    /// Tries both rustc lock styles on Unix for compatibility with older toolchains.
    fn try_acquire(path: &Path) -> io::Result<Self> {
        match metadata(path)? {
            Some(info) if info.is_file() => {}
            Some(_) => return Ok(Self::Unknown),
            // rustc creates and locks this file before creating the session directory.
            None => return Ok(Self::Missing),
        }
        let Some(file) = optional(at(
            path,
            OpenOptions::new().read(true).write(true).open(path),
        ))?
        else {
            return Ok(Self::Missing);
        };

        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(Self::Busy),
            Err(TryLockError::Error(error)) => return at(path, Err(error)),
        }
        #[cfg(unix)]
        match fcntl_lock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::ACCESS | Errno::AGAIN) => return Ok(Self::Busy),
            Err(error) => return at(path, Err(error.into())),
        }
        Ok(Self::Held { _file: file })
    }
}

impl SessionStats {
    /// Weights filesystem entries and, for removals, logical file size.
    const fn work(&self, removing: bool) -> u64 {
        let size_work = if removing {
            self.bytes.div_ceil(BYTES_PER_WORK_UNIT)
        } else {
            0
        };
        BASE_SESSION_WORK
            .saturating_add(self.entries)
            .saturating_add(size_work)
    }

    /// Inspects a complete session without following symlinks or accepting special files.
    fn read(path: &Path) -> io::Result<Option<Self>> {
        let Some(root) = metadata(path)?.filter(Metadata::is_dir) else {
            return Ok(None);
        };
        let mut stats = Self {
            bytes: 0,
            entries: 0,
            modified: at(path, root.modified())?,
        };
        let mut pending = vec![path.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Some(entries) = optional(at(&dir, fs::read_dir(&dir)))? else {
                return Ok(None);
            };
            for entry in entries {
                let entry = at(&dir, entry)?;
                let path = entry.path();
                let Some(info) = optional(at(&path, entry.metadata()))? else {
                    return Ok(None);
                };
                if info.is_dir() {
                    pending.push(path.clone());
                } else if info.is_file() {
                    stats.bytes = stats.bytes.saturating_add(info.len());
                } else {
                    return Ok(None);
                }
                stats.entries += 1;
                stats.modified = stats.modified.max(at(&path, info.modified())?);
            }
        }
        Ok(Some(stats))
    }
}

/// Lists real immediate subdirectories in a reproducible order.
fn directories(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if let Some(entries) = optional(at(root, fs::read_dir(root)))? {
        for entry in entries {
            let entry = at(root, entry)?;
            if optional(at(&entry.path(), entry.file_type()))?.is_some_and(|kind| kind.is_dir()) {
                paths.push(entry.path());
            }
        }
    }
    paths.sort_unstable();
    Ok(paths)
}

/// Validates the lowercase, fixed-width identifiers used in rustc cache names.
fn is_base36(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// Checks the age against one reference time, preserving future-dated entries.
fn is_old(modified: SystemTime, now: SystemTime, options: &Options) -> bool {
    now.duration_since(modified)
        .is_ok_and(|age| age >= options.age)
}

/// Reads metadata without following symlinks.
fn metadata(path: &Path) -> io::Result<Option<Metadata>> {
    optional(at(path, fs::symlink_metadata(path)))
}

/// Treats entries concurrently removed by Cargo or rustc as absent.
fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Adds the affected path to a filesystem error.
fn at<T>(path: &Path, result: io::Result<T>) -> io::Result<T> {
    result.map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}
