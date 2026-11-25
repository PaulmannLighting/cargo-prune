//! Render the real progress bar into a recording terminal while pruning fixture caches.

use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use std::{env, io, mem, process};

use clap::Parser;
use indicatif::{ProgressBar, ProgressDrawTarget, TermLike};

use crate::cache::{self, Report};
use crate::cli::Options;

const TERMINAL_WIDTH: u16 = 80;
const CRATE_CACHE: &str = "debug/incremental/example-0000000000000";
const OLDEST: &str = "s-0000000001-0000000-working";
const LOCKED: &str = "s-0000000002-0000000-working";
const NEWEST: &str = "s-0000000003-0000000-working";
const LOCK_FILE: &str = "s-0000000002-0000000.lock";
const SMALL_FILE_BYTES: u64 = 5;
const LOCKED_FILE_BYTES: u64 = 16 * 1024 * 1024;
const PRUNE_ARGS: [&str; 2] = ["--target=host", "--older-than=0"];

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// One rendered line and the ETA tracker's state when it was drawn.
#[derive(Debug)]
struct Frame {
    text: String,
    completed_work: u64,
    remaining_work: u64,
    eta: Duration,
}

/// The render history and the session lock to acquire at the processing transition.
#[derive(Debug)]
struct Recording {
    frames: Vec<Frame>,
    session_lock: File,
    locked: bool,
}

/// Records text from indicatif and synchronizes the lock with its first processing frame.
#[derive(Debug)]
struct RecordingTerminal {
    recording: Arc<Mutex<Recording>>,
    work: ProgressBar,
}

impl TermLike for RecordingTerminal {
    fn width(&self) -> u16 {
        TERMINAL_WIDTH
    }

    fn move_cursor_up(&self, _: usize) -> io::Result<()> {
        Ok(())
    }

    fn move_cursor_down(&self, _: usize) -> io::Result<()> {
        Ok(())
    }

    fn move_cursor_right(&self, _: usize) -> io::Result<()> {
        Ok(())
    }

    fn move_cursor_left(&self, _: usize) -> io::Result<()> {
        Ok(())
    }

    fn write_line(&self, text: &str) -> io::Result<()> {
        self.write_str(text)
    }

    fn write_str(&self, text: &str) -> io::Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let mut recording = self
            .recording
            .lock()
            .map_err(|_| io::Error::other("recording mutex poisoned"))?;
        if !recording.locked && text.contains(" sessions | ETA ") {
            // The initial processing frame is drawn after sizing and before the first removal.
            recording
                .session_lock
                .try_lock_shared()
                .map_err(io::Error::other)?;
            recording.locked = true;
        }

        let completed_work = self.work.position();
        let remaining_work = self
            .work
            .length()
            .unwrap_or_default()
            .saturating_sub(completed_work);
        recording.frames.push(Frame {
            text: text.to_owned(),
            completed_work,
            remaining_work,
            eta: self.work.eta(),
        });
        drop(recording);
        Ok(())
    }

    fn clear_line(&self) -> io::Result<()> {
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Isolated incremental cache used by one progress test.
struct Fixture(PathBuf);

impl Fixture {
    /// Creates the fixture root.
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = env::temp_dir().join(format!("cargo-prune-progress-{}-{id}", process::id()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    /// Creates a recognized session with a cache file of the requested size.
    fn session(&self, name: &str, bytes: u64) -> PathBuf {
        let session = self.0.join(CRATE_CACHE).join(name);
        fs::create_dir_all(&session).unwrap();
        File::create(session.join("dep-graph.bin"))
            .unwrap()
            .set_len(bytes)
            .unwrap();
        session
    }

    /// Prunes the fixture and records progress, locking the designated session after sizing.
    fn prune(&self) -> (Report, Vec<Frame>) {
        let options = Options::try_parse_from(PRUNE_ARGS).unwrap();
        let (display, work, style) = super::new().unwrap();
        let recording = Arc::new(Mutex::new(Recording {
            frames: Vec::new(),
            session_lock: File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(self.0.join(CRATE_CACHE).join(LOCK_FILE))
                .unwrap(),
            locked: false,
        }));
        display.set_draw_target(ProgressDrawTarget::term_like(Box::new(RecordingTerminal {
            recording: Arc::clone(&recording),
            work: work.clone(),
        })));
        let report =
            cache::prune(&self.0, &options, SystemTime::now(), &display, style, &work).unwrap();
        display.finish_and_clear();
        let mut recording = recording.lock().unwrap();
        assert!(
            recording.locked,
            "the initial processing frame must acquire the lock"
        );
        (report, mem::take(&mut recording.frames))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Finds a frame with both the expected session count and byte totals.
fn frame<'a>(frames: &'a [Frame], count: &str, totals: &str) -> &'a Frame {
    frames
        .iter()
        .find(|frame| frame.text.contains(count) && frame.text.contains(totals))
        .unwrap_or_else(|| panic!("missing {count:?} with {totals:?} in {frames:#?}"))
}

#[test]
fn drops_skipped_bytes_and_work_while_other_sessions_remain() {
    let fixture = Fixture::new();
    let newest = fixture.session(NEWEST, SMALL_FILE_BYTES);
    let locked = fixture.session(LOCKED, LOCKED_FILE_BYTES);
    let oldest = fixture.session(OLDEST, SMALL_FILE_BYTES);
    let (report, frames) = fixture.prune();

    assert!(frames[0].text.contains("Scanning incremental caches"));
    assert!(!frames[0].text.contains("ETA"));
    let start = frame(&frames, "0/3 sessions | ETA --", "0 removed (0 B/16.0 MiB)");
    let before_skip = frame(&frames, "1/3 sessions", "1 removed (5 B/16.0 MiB)");
    let after_skip = frame(&frames, "2/3 sessions", "1 removed (5 B/10 B)");
    let finish = frame(&frames, "3/3 sessions | ETA 0s", "2 removed (10 B/10 B)");

    assert_eq!(
        start.remaining_work,
        before_skip.completed_work + before_skip.remaining_work
    );
    assert_eq!(before_skip.completed_work, after_skip.completed_work);
    assert!(
        after_skip.remaining_work > 0 && after_skip.remaining_work < before_skip.remaining_work
    );
    assert_eq!(
        after_skip.completed_work + after_skip.remaining_work,
        finish.completed_work
    );
    assert_eq!(finish.remaining_work, 0);
    assert_eq!(report.bytes, 2 * SMALL_FILE_BYTES);
    assert_eq!(report.busy, 1);
    assert!(locked.exists() && !newest.exists() && !oldest.exists());
}

#[test]
fn eta_reaches_zero_when_the_last_session_is_skipped() {
    let fixture = Fixture::new();
    fixture.session(NEWEST, SMALL_FILE_BYTES);
    let locked = fixture.session(LOCKED, LOCKED_FILE_BYTES);
    let (report, frames) = fixture.prune();

    let before_skip = frame(&frames, "1/2 sessions", "1 removed (5 B/16.0 MiB)");
    let after_skip = frame(&frames, "2/2 sessions | ETA 0s", "1 removed (5 B/5 B)");
    // Assert that the estimate reaches zero without depending on a particular processing rate.
    assert!(before_skip.eta > Duration::ZERO);
    assert_eq!(after_skip.eta, Duration::ZERO);
    assert_eq!(before_skip.completed_work, after_skip.completed_work);
    assert_eq!(after_skip.remaining_work, 0);
    assert_eq!(report.sessions, 1);
    assert_eq!(report.busy, 1);
    assert!(locked.exists());
}
