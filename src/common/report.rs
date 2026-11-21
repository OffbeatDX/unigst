use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rayon::prelude::*;

#[derive(Debug, Default)]
pub struct CancelToken(AtomicBool);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub trait ProgressSink: Sync {
    fn started(&self, _title: &str) {}

    fn advanced(&self) {}

    fn warned(&self, _msg: &str) {}

    fn failed(&self, msg: &str) {
        self.warned(msg);
    }

    fn logged(&self, _msg: &str) {}
}

pub struct NullSink;

impl ProgressSink for NullSink {}

#[derive(Debug, Clone)]
pub struct PlannedTrack {
    pub id: String,

    pub title: String,
    pub artist: String,

    pub output: PathBuf,
}

impl PlannedTrack {
    /// One-line listing used by `--dry-run`: `<id>. Artist - Title`
    /// (`Artist - Title` when the song has no id).
    pub fn summary_line(&self) -> String {
        if self.id.is_empty() {
            format!("{} - {}", self.artist, self.title)
        } else {
            format!("{}. {} - {}", self.id, self.artist, self.title)
        }
    }
}

/// One selectable song (several planned tracks may share it, e.g. SDVX
/// per-difficulty audio). Used by the GUI's song picker.
#[derive(Debug, Clone)]
pub struct SongEntry {
    /// Value for `--id`; `None` when the song can't be selected individually.
    pub id: Option<String>,
    pub title: String,
    pub artist: String,
    /// Genre(s) as shown to the user; empty when unknown.
    pub genre: String,
    /// Value for `-v/--version`; `None` when unknown.
    pub version: Option<String>,
    /// Human-readable version name, e.g. "6 – EXCEED GEAR".
    pub version_label: String,
    /// Numeric sort key for the version.
    pub version_order: u32,
    /// Number of output files this song produces.
    pub tracks: usize,
}

#[derive(Debug, Default)]
pub struct RunReport {
    pub encoded: usize,

    pub skipped: usize,
    pub failures: Vec<String>,
    pub warnings: Vec<String>,

    pub cancelled: bool,
}

#[derive(Default)]
pub(crate) struct Collector {
    encoded: AtomicUsize,
    skipped: AtomicUsize,
    failures: Mutex<Vec<String>>,
    warnings: Mutex<Vec<String>>,
}

impl Collector {
    pub fn ok(&self) {
        self.encoded.fetch_add(1, Ordering::Relaxed);
    }

    pub fn skip(&self, msg: String, sink: &dyn ProgressSink) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
        sink.logged(&msg);
    }

    pub fn warn(&self, msg: String, sink: &dyn ProgressSink) {
        sink.warned(&msg);
        self.warnings.lock().unwrap().push(msg);
    }

    pub fn fail(&self, msg: String, sink: &dyn ProgressSink) {
        sink.failed(&msg);
        self.failures.lock().unwrap().push(msg);
    }

    pub fn into_report(self, cancelled: bool) -> RunReport {
        RunReport {
            encoded: self.encoded.into_inner(),
            skipped: self.skipped.into_inner(),
            failures: self.failures.into_inner().unwrap(),
            warnings: self.warnings.into_inner().unwrap(),
            cancelled,
        }
    }
}

pub(crate) fn run_jobs<J, F>(
    jobs: &[J],
    threads: usize,
    warnings: &[String],
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
    process: F,
) -> Result<RunReport>
where
    J: Sync,
    F: Fn(&J, &Collector, &dyn ProgressSink) + Sync,
{
    let collector = Collector::default();
    for w in warnings {
        collector.warn(w.clone(), sink);
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .exit_handler(|_| crate::common::formats::mf::shutdown_thread())
        .build()
        .context("failed to build thread pool")?;

    pool.install(|| {
        jobs.par_iter().for_each(|job| {
            if cancel.is_cancelled() {
                return;
            }
            process(job, &collector, sink);
            sink.advanced();
        });
    });

    Ok(collector.into_report(cancel.is_cancelled()))
}

pub trait Plan {
    fn tracks(&self) -> Vec<PlannedTrack>;

    fn warnings(&self) -> &[String];

    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Per-item diagnostics for `--dry-run --verbose`.
    fn detail_lines(&self) -> Vec<String> {
        self.tracks()
            .iter()
            .map(|t| t.output.display().to_string())
            .collect()
    }

    fn dry_run_summary(&self) -> String;

    /// Exportable songs, one entry per song id, in plan order.
    fn songs(&self) -> Vec<SongEntry>;
}

pub fn tracks_to_json(tracks: &[PlannedTrack]) -> String {
    use std::fmt::Write;
    let mut out = String::from("[\n");
    for (i, t) in tracks.iter().enumerate() {
        let comma = if i + 1 < tracks.len() { "," } else { "" };
        let _ = writeln!(
            out,
            "  {{\"id\": {}, \"title\": {}, \"artist\": {}, \"output\": {}}}{comma}",
            json_str(&t.id),
            json_str(&t.title),
            json_str(&t.artist),
            json_str(&t.output.display().to_string()),
        );
    }
    out.push(']');
    out
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
