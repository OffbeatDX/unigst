//! Minimal front-end over the same `plan`/`execute` pipeline the CLI uses.
//! Arguments are assembled as an argv and parsed by the CLI's own clap
//! definitions, so validation and defaults stay in one place.
//!
//! Flow: pick a game folder → the game is detected and the folder scanned (same
//! `plan()` the CLI runs, with no filters) → pick a version and songs → export.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use serde::{Deserialize, Serialize};

use unigst::cli::{Cli, Command};
use unigst::common::report::{CancelToken, Plan, ProgressSink, RunReport, SongEntry};
use unigst::{iidx, popn, sdvx};

const SETTINGS_KEY: &str = "settings";
/// Bump to migrate persisted settings (see `App::new`).
const SETTINGS_VERSION: u32 = 3;

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("unigst GUI {}", unigst::VERSION))
            .with_inner_size([900.0, 760.0])
            .with_min_inner_size([640.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "unigst",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)?))),
    )
}

// ---------------------------------------------------------------------------
// Settings (persisted between launches)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
enum Game {
    #[default]
    Sdvx,
    Iidx,
    Popn,
}

impl Game {
    /// Detect the game from its folder: each game needs both its DLL and its
    /// song data, so unsupported games aren't mistaken for supported ones.
    fn detect(input: &Path) -> Option<Game> {
        if sdvx::looks_like(input) {
            Some(Game::Sdvx)
        } else if iidx::looks_like(input) {
            Some(Game::Iidx)
        } else if popn::looks_like(input) {
            Some(Game::Popn)
        } else {
            None
        }
    }

    fn label(self) -> &'static str {
        match self {
            Game::Sdvx => "SOUND VOLTEX",
            Game::Iidx => "beatmania IIDX",
            Game::Popn => "pop'n music",
        }
    }

    fn subcommand(self) -> &'static str {
        match self {
            Game::Sdvx => "sdvx",
            Game::Iidx => "iidx",
            Game::Popn => "popn",
        }
    }
}

/// Which versions the song list shows. Persisted as a string:
/// "" = all, "?" = unknown, anything else = a version key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionFilter {
    All,
    Unknown,
    Key(String),
}

impl VersionFilter {
    fn from_saved(s: &str) -> Self {
        match s {
            "" => VersionFilter::All,
            "?" => VersionFilter::Unknown,
            k => VersionFilter::Key(k.to_string()),
        }
    }

    fn to_saved(&self) -> String {
        match self {
            VersionFilter::All => String::new(),
            VersionFilter::Unknown => "?".into(),
            VersionFilter::Key(k) => k.clone(),
        }
    }

    /// `unknown_in_all`: whether "All Versions" includes songs with no known
    /// version. Normally not (they're mostly removed songs that left data on
    /// disk); only when *no* song has a known version.
    fn matches(&self, song: &SongEntry, unknown_in_all: bool) -> bool {
        match self {
            VersionFilter::All => unknown_in_all || song.version.is_some(),
            VersionFilter::Unknown => song.version.is_none(),
            VersionFilter::Key(k) => song.version.as_deref() == Some(k.as_str()),
        }
    }
}

/// Song table column the list is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum SortCol {
    #[default]
    Id,
    Title,
    Artist,
    Genre,
    Version,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct GameFields {
    /// Per-game folder from settings v2 and earlier; migrated to
    /// `Settings::input` and no longer written.
    #[serde(rename = "input", skip_serializing)]
    legacy_input: String,
    /// Last chosen version filter (see `VersionFilter::from_saved`).
    version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    /// Missing in settings saved before versioning → 0 (not `Default`'s value).
    #[serde(default)]
    settings_version: u32,
    /// Game folder (the game is detected from it).
    input: String,
    /// Last detected game; only used to migrate old settings.
    game: Game,
    sdvx: GameFields,
    iidx: GameFields,
    popn: GameFields,
    output: String,
    format: String,
    jobs: usize,
    overwrite: bool,
    verbose: bool,
    sort_col: SortCol,
    sort_desc: bool,
    /// Song table column widths as shares of the table width, per column
    /// layout (see `App::table`), so they survive window resizes and restarts.
    col_shares: BTreeMap<String, Vec<f32>>,
}

impl Default for Settings {
    fn default() -> Self {
        let output = std::env::current_dir()
            .map(|d| d.join("GST").display().to_string())
            .unwrap_or_else(|_| "GST".into());
        Settings {
            settings_version: SETTINGS_VERSION,
            input: String::new(),
            game: Game::default(),
            sdvx: GameFields::default(),
            iidx: GameFields::default(),
            popn: GameFields::default(),
            output,
            format: "opus".into(),
            jobs: 0,
            overwrite: false,
            verbose: false,
            sort_col: SortCol::Id,
            sort_desc: false,
            col_shares: BTreeMap::new(),
        }
    }
}

impl Settings {
    fn fields(&self, game: Game) -> &GameFields {
        match game {
            Game::Sdvx => &self.sdvx,
            Game::Iidx => &self.iidx,
            Game::Popn => &self.popn,
        }
    }

    fn fields_mut(&mut self, game: Game) -> &mut GameFields {
        match game {
            Game::Sdvx => &mut self.sdvx,
            Game::Iidx => &mut self.iidx,
            Game::Popn => &mut self.popn,
        }
    }

    /// What identifies a scan: changing any of these rescans the game folder.
    fn scan_key(&self) -> ScanKey {
        ScanKey {
            input: self.input.trim().to_string(),
        }
    }

    /// Minimal argv for scanning the input folder (independent of output/format).
    fn scan_argv(&self, game: Game) -> Vec<OsString> {
        vec![
            "unigst".into(),
            game.subcommand().into(),
            format!("--input={}", self.input.trim()).into(),
            "--dry-run".into(),
        ]
    }

    /// Arguments for exporting. Uses `--flag=value` for paths so names
    /// starting with `-` can never be misparsed as flags.
    fn base_argv(&self, game: Game) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec![
            "unigst".into(),
            game.subcommand().into(),
            format!("--input={}", self.input.trim()).into(),
        ];
        let output = self.output.trim();
        if !output.is_empty() {
            a.push(format!("--output={output}").into());
        }
        a.push("-f".into());
        a.push(self.format.clone().into());
        if self.jobs > 0 {
            a.push("-j".into());
            a.push(self.jobs.to_string().into());
        }
        if self.overwrite {
            a.push("--overwrite".into());
        }
        a
    }
}

fn quote_argv(argv: &[OsString]) -> String {
    const SHELL_META: &[char] = &[
        '"', '&', '|', '<', '>', '^', '%', '(', ')', '!', ';', '$', '\'', '`',
    ];
    argv.iter()
        .map(|a| {
            let s = a.to_string_lossy();
            if s.is_empty() || s.contains(|c: char| c.is_whitespace() || SHELL_META.contains(&c)) {
                let mut out = String::with_capacity(s.len() + 2);
                out.push('"');
                let mut backslashes = 0usize;
                for c in s.chars() {
                    if c == '\\' {
                        backslashes += 1;
                        out.push('\\');
                    } else if c == '"' {
                        for _ in 0..backslashes {
                            out.push('\\');
                        }
                        backslashes = 0;
                        out.push_str("\\\"");
                    } else {
                        backslashes = 0;
                        out.push(c);
                    }
                }
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push('"');
                out
            } else {
                s.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn clap_error(e: clap::Error) -> String {
    // Keep clap's message, drop its usage/help trailer.
    let msg = e.to_string();
    let msg = msg.split("\n\n").next().unwrap_or(&msg);
    msg.trim_start_matches("error: ").trim().to_string()
}

// ---------------------------------------------------------------------------
// Scanning the game folder

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanKey {
    input: String,
}

struct VersionInfo {
    filter: VersionFilter,
    label: String,
    count: usize,
}

/// Precomputed sort keys, parallel to `Catalog::songs`.
struct SortKeys {
    id: (u64, String),
    title: String,
    artist: String,
    genre: String,
}

struct Catalog {
    songs: Vec<SongEntry>,
    /// Songs with / without a known version.
    known: usize,
    unknown: usize,
    /// Any song has an ID (otherwise individual selection isn't possible, e.g.
    /// pop'n fallback mode when the DLL db couldn't be parsed).
    has_ids: bool,
    keys: Vec<SortKeys>,
    /// Any song has a genre: show the Genre column.
    has_genre: bool,
    versions: Vec<VersionInfo>,
    warnings: Vec<String>,
}

impl Catalog {
    /// See `VersionFilter::matches`.
    fn unknown_in_all(&self) -> bool {
        self.known == 0
    }

    /// Number of songs listed under "All Versions".
    fn all_count(&self) -> usize {
        if self.unknown_in_all() {
            self.songs.len()
        } else {
            self.known
        }
    }

    fn new(songs: Vec<SongEntry>, warnings: Vec<String>) -> Self {
        let mut by_version: BTreeMap<(u32, Option<String>), (String, usize)> = BTreeMap::new();
        for s in &songs {
            let e = by_version
                .entry((s.version_order, s.version.clone()))
                .or_insert_with(|| (s.version_label.clone(), 0));
            e.1 += 1;
        }
        let versions = by_version
            .into_iter()
            .map(|((_, key), (label, count))| VersionInfo {
                filter: match key {
                    Some(k) => VersionFilter::Key(k),
                    None => VersionFilter::Unknown,
                },
                label,
                count,
            })
            .collect();
        let keys = songs
            .iter()
            .map(|s| SortKeys {
                // Numeric ids sort numerically; anything else after them.
                id: match s.id.as_deref() {
                    Some(id) => (id.parse().unwrap_or(u64::MAX), id.to_string()),
                    None => (u64::MAX, String::new()),
                },
                title: natural_key(&s.title),
                artist: natural_key(&s.artist),
                genre: natural_key(&s.genre),
            })
            .collect();
        let has_genre = songs.iter().any(|s| !s.genre.is_empty());
        let has_ids = songs.iter().any(|s| s.id.is_some());
        let known = songs.iter().filter(|s| s.version.is_some()).count();
        Catalog {
            unknown: songs.len() - known,
            known,
            has_ids,
            songs,
            keys,
            has_genre,
            versions,
            warnings,
        }
    }
}

enum ScanState {
    Idle,
    Scanning {
        key: ScanKey,
        result: Arc<Mutex<Option<ScanResult>>>,
    },
    Ready {
        key: ScanKey,
        game: Game,
        catalog: Catalog,
    },
    Failed {
        key: ScanKey,
        /// Set when the game was recognised but its data couldn't be read.
        game: Option<Game>,
        error: String,
    },
}

type ScanResult = (Option<Game>, Result<Catalog, String>);

impl ScanState {
    fn game(&self) -> Option<Game> {
        match self {
            ScanState::Ready { game, .. } => Some(*game),
            ScanState::Failed { game, .. } => *game,
            _ => None,
        }
    }

    fn key(&self) -> Option<&ScanKey> {
        match self {
            ScanState::Idle => None,
            ScanState::Scanning { key, .. }
            | ScanState::Ready { key, .. }
            | ScanState::Failed { key, .. } => Some(key),
        }
    }
}

/// Detect the game, then plan with no filters to list every exportable song.
fn scan(settings: &Settings) -> ScanResult {
    fn collect<P: Plan>(plan: Result<P>) -> Result<Catalog, String> {
        plan.map(|p| Catalog::new(p.songs(), p.warnings().to_vec()))
            .map_err(|e| format!("{e:#}"))
    }
    let input = Path::new(settings.input.trim());
    if !input.is_dir() {
        return (None, Err(format!("folder not found: {}", input.display())));
    }
    let Some(game) = Game::detect(input) else {
        return (
            None,
            Err(format!(
                "not a supported game folder. Expected one of:\n\
                 • SOUND VOLTEX: {} + data/others/music_db.xml\n\
                 • beatmania IIDX: {} + data/info/…/music_data.bin\n\
                 • pop'n music: {} + data/sd/\n\
                 (DLLs in modules/ or the folder itself)",
                sdvx::GAME_DLL,
                iidx::GAME_DLL,
                popn::GAME_DLL,
            )),
        );
    };
    let argv = settings.scan_argv(game);
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(e) => return (Some(game), Err(clap_error(e))),
    };
    let catalog = match cli.command {
        Command::Sdvx(args) => collect(sdvx::plan(&args)),
        Command::Iidx(args) => collect(iidx::plan(&args)),
        Command::Popn(args) => collect(popn::plan(&args)),
    };
    (Some(game), catalog)
}

// ---------------------------------------------------------------------------
// Worker <-> UI shared state for export / preview runs

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Info,
    Warn,
    Error,
    Summary,
}

struct Shared {
    ctx: egui::Context,
    cancel: CancelToken,
    total: AtomicU64,
    done: AtomicU64,
    planning: AtomicBool,
    finished: AtomicBool,
    verbose: bool,
    current: Mutex<String>,
    log: Mutex<Vec<(Level, String)>>,
}

impl Shared {
    fn push(&self, level: Level, msg: impl Into<String>) {
        let msg = msg.into();
        let mut log = self.log.lock().unwrap();
        let mut any = false;
        for line in msg.lines() {
            log.push((level, line.to_string()));
            any = true;
        }
        if !any {
            log.push((level, String::new()));
        }
        drop(log);
        self.ctx.request_repaint_after(Duration::from_millis(100));
    }
}

struct GuiSink<'a>(&'a Shared);

impl ProgressSink for GuiSink<'_> {
    fn started(&self, title: &str) {
        *self.0.current.lock().unwrap() = title.to_string();
        self.0.ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn advanced(&self) {
        self.0.done.fetch_add(1, Ordering::Relaxed);
        self.0.ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn warned(&self, msg: &str) {
        self.0.push(Level::Warn, msg);
    }

    fn failed(&self, msg: &str) {
        self.0.push(Level::Error, msg);
    }

    fn logged(&self, msg: &str) {
        if self.0.verbose {
            self.0.push(Level::Info, msg);
        }
    }
}

fn worker(cli: Cli, shared: &Shared) -> Result<()> {
    match cli.command {
        Command::Sdvx(args) => drive(sdvx::plan(&args), shared, |p, s, c| {
            sdvx::execute(&args, p, s, c)
        }),
        Command::Iidx(args) => drive(iidx::plan(&args), shared, |p, s, c| {
            iidx::execute(&args, p, s, c)
        }),
        Command::Popn(args) => drive(popn::plan(&args), shared, |p, s, c| {
            popn::execute(&args, p, s, c)
        }),
    }
}

fn drive<P, F>(plan: Result<P>, shared: &Shared, execute: F) -> Result<()>
where
    P: Plan,
    F: FnOnce(&P, &dyn ProgressSink, &CancelToken) -> Result<RunReport>,
{
    let plan = plan?;
    shared.planning.store(false, Ordering::Relaxed);

    if plan.is_empty() {
        for w in plan.warnings() {
            shared.push(Level::Warn, w.clone());
        }
        shared.push(
            Level::Summary,
            "no songs matched the selection — nothing to do",
        );
        return Ok(());
    }

    let started = Instant::now();
    shared.total.store(plan.len() as u64, Ordering::Relaxed);
    let report = execute(&plan, &GuiSink(shared), &shared.cancel)?;

    if !report.failures.is_empty() {
        shared.push(
            Level::Error,
            format!("{} failure(s):", report.failures.len()),
        );
        for f in &report.failures {
            shared.push(Level::Error, format!("  {f}"));
        }
    }
    let mut summary = format!("done: {}/{} encoded", report.encoded, plan.len());
    if report.skipped > 0 {
        summary.push_str(&format!(", {} skipped (already existed)", report.skipped));
    }
    if !report.warnings.is_empty() {
        summary.push_str(&format!(", {} warning(s)", report.warnings.len()));
    }
    if !report.failures.is_empty() {
        summary.push_str(&format!(", {} FAILED", report.failures.len()));
    }
    if report.cancelled {
        summary.push_str(", cancelled");
    }
    summary.push_str(&format!(
        " — {}",
        indicatif::HumanDuration(started.elapsed())
    ));
    shared.push(Level::Summary, summary);
    Ok(())
}

// ---------------------------------------------------------------------------
// App

struct Job {
    shared: Arc<Shared>,
}

struct App {
    settings: Settings,
    scan: ScanState,
    /// Version filter of the detected game (mirrors its `GameFields::version`).
    version: VersionFilter,
    search: String,
    /// Selected song ids.
    selected: HashSet<String>,
    /// Row index (into the listed songs) of the last plain click, for shift-click ranges.
    anchor: Option<usize>,
    /// The input path text field has focus: don't rescan on every keystroke.
    editing_path: bool,
    /// Width and layout the song table's columns were last fitted to.
    table_fit: (f32, String),
    job: Option<Job>,
    /// Last finished job, kept so its log stays visible.
    last: Option<Job>,
    close_when_done: bool,
    form_error: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        install_fonts(&cc.egui_ctx)?;
        let mut settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();
        if settings.settings_version < 2 {
            // v2: Opus became the default format.
            settings.format = "opus".into();
        }
        if settings.settings_version < 3 {
            // v3: one game folder instead of one per game tab.
            settings.input = settings.fields(settings.game).legacy_input.clone();
        }
        settings.settings_version = SETTINGS_VERSION;
        Ok(App {
            settings,
            scan: ScanState::Idle,
            version: VersionFilter::All,
            search: String::new(),
            selected: HashSet::new(),
            anchor: None,
            editing_path: false,
            table_fit: (0.0, String::new()),
            job: None,
            last: None,
            close_when_done: false,
            form_error: None,
        })
    }

    fn running(&self) -> bool {
        self.job.is_some()
    }

    fn catalog(&self) -> Option<&Catalog> {
        match &self.scan {
            ScanState::Ready { catalog, .. } => Some(catalog),
            _ => None,
        }
    }

    fn set_version(&mut self, v: VersionFilter) {
        if self.version != v {
            self.version = v;
            if let Some(game) = self.scan.game() {
                self.settings.fields_mut(game).version = self.version.to_saved();
            }
            // Keep only selections that are still listed under the new version.
            if let Some(cat) = self.catalog() {
                let keep: HashSet<String> = cat
                    .songs
                    .iter()
                    .filter(|s| self.version.matches(s, cat.unknown_in_all()))
                    .filter_map(|s| s.id.clone())
                    .filter(|id| self.selected.contains(id))
                    .collect();
                self.selected = keep;
            }
            self.anchor = None;
        }
    }

    /// Songs shown in the table: version filter + search.
    fn listed(&self) -> Vec<&SongEntry> {
        let Some(cat) = self.catalog() else {
            return Vec::new();
        };
        let needle = self.search.trim().to_lowercase();
        let mut idx: Vec<usize> = (0..cat.songs.len())
            .filter(|&i| self.version.matches(&cat.songs[i], cat.unknown_in_all()))
            .filter(|&i| {
                let s = &cat.songs[i];
                needle.is_empty()
                    || s.title.to_lowercase().contains(&needle)
                    || s.artist.to_lowercase().contains(&needle)
                    || s.genre.to_lowercase().contains(&needle)
                    || s.id.as_deref().is_some_and(|id| id == needle)
            })
            .collect();

        let (col, desc) = (self.settings.sort_col, self.settings.sort_desc);
        idx.sort_by(|&a, &b| {
            let (ka, kb) = (&cat.keys[a], &cat.keys[b]);
            let ord = match col {
                SortCol::Id => ka.id.cmp(&kb.id),
                SortCol::Title => ka.title.cmp(&kb.title),
                SortCol::Artist => ka.artist.cmp(&kb.artist),
                SortCol::Genre => ka.genre.cmp(&kb.genre),
                SortCol::Version => cat.songs[a].version_order.cmp(&cat.songs[b].version_order),
            };
            // Ties (same artist, genre, version…) fall back to ID order.
            let ord = ord.then_with(|| ka.id.cmp(&kb.id));
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
        idx.into_iter().map(|i| &cat.songs[i]).collect()
    }

    // --- scanning ----------------------------------------------------------

    /// Start a scan when the game/folder changed and the user isn't mid-edit.
    fn maybe_rescan(&mut self, ctx: &egui::Context) {
        if self.editing_path || self.running() {
            return;
        }
        let key = self.settings.scan_key();
        if self.scan.key() == Some(&key) {
            return;
        }
        self.selected.clear();
        self.anchor = None;
        self.search.clear();

        if key.input.is_empty() {
            self.scan = ScanState::Idle;
            return;
        }
        let settings = self.settings.clone();
        let result = Arc::new(Mutex::new(None));
        let slot = result.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| scan(&settings)))
                .unwrap_or_else(|_| (None, Err("internal error while scanning (panic)".into())));
            *slot.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        self.scan = ScanState::Scanning { key, result };
    }

    fn poll_scan(&mut self) {
        let ScanState::Scanning { key, result } = &self.scan else {
            return;
        };
        let Some(r) = result.lock().unwrap().take() else {
            return;
        };
        let key = key.clone();
        let (game, r) = r;
        if let Some(game) = game {
            self.settings.game = game;
        }
        self.scan = match (game, r) {
            (Some(game), Ok(catalog)) => {
                // Restore this game's last version; fall back to all if it's gone.
                self.version = VersionFilter::from_saved(&self.settings.fields(game).version);
                if self.version != VersionFilter::All
                    && !catalog.versions.iter().any(|v| v.filter == self.version)
                {
                    self.version = VersionFilter::All;
                }
                ScanState::Ready { key, game, catalog }
            }
            (game, Err(error)) => ScanState::Failed { key, game, error },
            (None, Ok(_)) => unreachable!("a catalog implies a detected game"),
        };
    }

    // --- export / preview --------------------------------------------------

    /// Full argv for exporting the selected songs, or a reason it can't run.
    ///
    /// A selection that is exactly every song of the chosen version (or every
    /// song) is passed as `-v` (or no filter) instead of a long `--id` list.
    fn export_argv(&self) -> Result<Vec<OsString>, String> {
        let (Some(cat), Some(game)) = (self.catalog(), self.scan.game()) else {
            return Err("Pick a valid game folder first.".into());
        };
        if cat.songs.is_empty() {
            return Err("No songs found to export.".into());
        }
        let mut argv = self.settings.base_argv(game);
        if self.settings.verbose {
            argv.push("--verbose".into());
        }
        // Fallback mode (e.g. pop'n with an unparseable DLL): songs have no IDs,
        // so export everything without filters.
        if !cat.has_ids {
            return Ok(argv);
        }
        if self.selected.is_empty() {
            return Err("Select songs to export.".into());
        }

        let in_version: Vec<&SongEntry> = cat
            .songs
            .iter()
            .filter(|s| self.version.matches(s, cat.unknown_in_all()))
            .collect();
        let whole_version = in_version.len() == self.selected.len()
            && in_version
                .iter()
                .all(|s| s.id.as_ref().is_some_and(|id| self.selected.contains(id)));
        if whole_version {
            match &self.version {
                // No filter would also export the unknown/removed songs the
                // GUI hides from "All Versions", so only when there are none.
                VersionFilter::All if cat.unknown == 0 || cat.unknown_in_all() => {
                    return Ok(argv);
                }
                VersionFilter::All => {}
                VersionFilter::Key(k) => {
                    argv.push("-v".into());
                    argv.push(k.into());
                    return Ok(argv);
                }
                VersionFilter::Unknown => {}
            }
        }

        // `--id=X` so an id can never be mistaken for a flag. Catalog order.
        for id in cat.songs.iter().filter_map(|s| s.id.as_ref()) {
            if self.selected.contains(id) {
                argv.push(format!("--id={id}").into());
            }
        }
        Ok(argv)
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.form_error = None;
        let argv = match self.export_argv() {
            Ok(a) => a,
            Err(e) => {
                self.form_error = Some(e);
                return;
            }
        };
        let cli = match Cli::try_parse_from(&argv) {
            Ok(cli) => cli,
            Err(e) => {
                self.form_error = Some(clap_error(e));
                return;
            }
        };

        let shared = Arc::new(Shared {
            ctx: ctx.clone(),
            cancel: CancelToken::new(),
            total: AtomicU64::new(0),
            done: AtomicU64::new(0),
            planning: AtomicBool::new(true),
            finished: AtomicBool::new(false),
            verbose: self.settings.verbose,
            current: Mutex::new(String::new()),
            log: Mutex::new(vec![(Level::Info, format!("> {}", quote_argv(&argv)))]),
        });

        let s = shared.clone();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| worker(cli, &s)));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(err)) => s.push(Level::Error, format!("error: {err:#}")),
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    s.push(Level::Error, format!("internal error (panic): {msg}"));
                }
            }
            s.planning.store(false, Ordering::Relaxed);
            s.finished.store(true, Ordering::Release);
            s.ctx.request_repaint();
        });

        self.last = None;
        self.job = Some(Job { shared });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let finished = self
            .job
            .as_ref()
            .is_some_and(|j| j.shared.finished.load(Ordering::Acquire));
        if finished {
            self.last = self.job.take();
            if self.close_when_done {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // Closing mid-run would orphan ffmpeg and leave .part files behind:
        // cancel first, close once in-flight tracks finish. A second close forces it.
        if ctx.input(|i| i.viewport().close_requested()) {
            if let Some(job) = &self.job {
                if !self.close_when_done {
                    job.shared.cancel.cancel();
                    job.shared.push(
                        Level::Warn,
                        "cancelling — window will close once in-flight tracks finish (close again to force)",
                    );
                    self.close_when_done = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            }
        }
    }

    // --- UI sections -------------------------------------------------------

    fn form(&mut self, ui: &mut egui::Ui) {
        let enabled = !self.running();
        let s = &mut self.settings;
        let mut editing_path = false;

        ui.add_enabled_ui(enabled, |ui| {
            egui::Grid::new("form")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Game folder");
                    let start = s.input.clone();
                    let r = path_row(
                        ui,
                        &mut s.input,
                        "SDVX / IIDX / pop'n game folder (contains data/)",
                        || dialog_in(&start).pick_folder(),
                    );
                    editing_path |= r.has_focus();
                    ui.end_row();

                    ui.label("Output folder");
                    let start = s.output.clone();
                    path_row(
                        ui,
                        &mut s.output,
                        "default: GST (relative to working dir)",
                        || dialog_in(&start).pick_folder(),
                    );
                    ui.end_row();

                    ui.label("Format");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("format")
                            .selected_text(format_label(&s.format))
                            .show_ui(ui, |ui| {
                                for f in ["opus", "flac", "mp3"] {
                                    ui.selectable_value(
                                        &mut s.format,
                                        f.to_string(),
                                        format_label(f),
                                    );
                                }
                            });
                        ui.add_space(16.0);
                        ui.label("Jobs");
                        ui.add(egui::DragValue::new(&mut s.jobs).range(0..=256));
                        let auto = std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(1);
                        ui.weak(format!("0 = auto ({auto})"));
                        ui.add_space(16.0);
                        ui.checkbox(&mut s.overwrite, "Overwrite existing")
                            .on_hover_text("Re-encode even if the output file exists");
                        ui.checkbox(&mut s.verbose, "Verbose log")
                            .on_hover_text("Also log skipped/per-item messages");
                    });
                    ui.end_row();
                });
        });
        self.editing_path = editing_path;
    }

    /// Scan status, version picker, search and the song table.
    fn songs(&mut self, ui: &mut egui::Ui) {
        let err = ui.visuals().error_fg_color;
        let warn = ui.visuals().warn_fg_color;
        match &self.scan {
            ScanState::Idle => {
                ui.weak("Pick the game folder to load its songs.");
                return;
            }
            ScanState::Scanning { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking folder and loading the song database…");
                });
                return;
            }
            ScanState::Failed { error, game, .. } => {
                let msg = match game {
                    Some(g) => format!("{}: {error}", g.label()),
                    None => {
                        let mut c = error.chars();
                        c.next()
                            .map(|f| f.to_uppercase().chain(c).collect())
                            .unwrap_or_default()
                    }
                };
                ui.colored_label(err, msg);
                return;
            }
            ScanState::Ready { .. } => {}
        }

        let enabled = !self.running();
        let (total, hidden, versions, warnings) = {
            let cat = self.catalog().unwrap();
            let versions: Vec<(VersionFilter, String)> = cat
                .versions
                .iter()
                .map(|v| (v.filter.clone(), format!("{} ({})", v.label, v.count)))
                .collect();
            let hidden = if cat.unknown_in_all() { 0 } else { cat.unknown };
            (cat.all_count(), hidden, versions, cat.warnings.clone())
        };

        ui.horizontal(|ui| {
            let game = self.scan.game().map(Game::label).unwrap_or_default();
            ui.colored_label(
                ui.visuals().strong_text_color(),
                format!("{game} · {total} songs found"),
            );
            if hidden > 0 {
                ui.weak(format!("(+{hidden} unknown/removed)"))
                    .on_hover_text(
                        "Song data on disk without a database entry, usually removed songs. \
                         Listed under the Unknown/Removed version.",
                    );
            }
            if !warnings.is_empty() {
                ui.colored_label(warn, format!("{} warning(s)", warnings.len()))
                    .on_hover_text(warnings.join("\n"));
            }
        });

        let mut new_version = None;
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Version");
                let current = if self.version == VersionFilter::All {
                    format!("All Versions ({total})")
                } else {
                    versions
                        .iter()
                        .find(|(f, _)| *f == self.version)
                        .map(|(_, l)| l.clone())
                        .unwrap_or_default()
                };
                egui::ComboBox::from_id_salt("version")
                    .selected_text(current)
                    .width(260.0)
                    .height(400.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                self.version == VersionFilter::All,
                                format!("All Versions ({total})"),
                            )
                            .clicked()
                        {
                            new_version = Some(VersionFilter::All);
                        }
                        for (f, label) in &versions {
                            if ui.selectable_label(self.version == *f, label).clicked() {
                                new_version = Some(f.clone());
                            }
                        }
                    });

                ui.add_space(12.0);
                ui.label("Search");
                let search_changed = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("title, artist, genre or ID")
                            .desired_width(220.0),
                    )
                    .changed();
                let cleared = !self.search.is_empty() && ui.small_button("×").clicked();
                if cleared {
                    self.search.clear();
                }
                if search_changed || cleared {
                    self.anchor = None;
                }
            });
        });
        if let Some(v) = new_version {
            self.set_version(v);
        }

        let listed: Vec<SongEntry> = self.listed().into_iter().cloned().collect();

        ui.add_space(4.0);

        self.table(ui, &listed, enabled);
    }

    fn table(&mut self, ui: &mut egui::Ui, listed: &[SongEntry], enabled: bool) {
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let shift = ui.input(|i| i.modifiers.shift);
        let show_version = self.version == VersionFilter::All;
        let show_genre = self.catalog().is_some_and(|c| c.has_genre);
        let mut clicked: Option<usize> = None;
        let sort = (self.settings.sort_col, self.settings.sort_desc);
        let mut sort_clicked: Option<SortCol> = None;

        // Columns after the checkbox: ID, Title, Artist, [Genre], [Version] as
        // (default share, minimum width). Widths are kept as shares of the
        // table width: when the window is resized the table is re-fitted from
        // them, so nothing ends up off-screen and manual adjustments survive.
        // The last column takes whatever is left.
        let mut cols: Vec<(f32, f32)> = vec![(0.08, 40.0), (0.38, 120.0), (0.28, 100.0)];
        if show_genre {
            cols.push((0.12, 70.0));
        }
        if show_version {
            cols.push((0.16, 110.0));
        }
        let n = cols.len();
        let layout = format!("{}{}", u8::from(show_genre), u8::from(show_version));
        let shares: Vec<f32> = match self.settings.col_shares.get(&layout) {
            Some(v) if v.len() == n && v.iter().all(|x| x.is_finite() && *x > 0.0) => v.clone(),
            _ => {
                let sum: f32 = cols.iter().map(|c| c.0).sum();
                cols.iter().map(|c| c.0 / sum).collect()
            }
        };
        const CHECKBOX_W: f32 = 22.0;
        let spacing = ui.spacing().item_spacing.x * n as f32;
        let free =
            (ui.available_width() - ui.spacing().scroll.allocated_width() - CHECKBOX_W - spacing)
                .max(1.0);
        let refit = (free - self.table_fit.0).abs() > 0.5 || self.table_fit.1 != layout;
        self.table_fit = (free, layout.clone());

        let mut table = TableBuilder::new(ui)
            .id_salt(("songs", &layout))
            .striped(true)
            .sense(egui::Sense::click())
            .auto_shrink([false, false])
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(CHECKBOX_W));
        if refit {
            table.reset();
        }
        for (i, (&(_, min), share)) in cols.iter().zip(&shares).enumerate() {
            let col = if i == n - 1 {
                Column::remainder()
            } else {
                Column::initial((share * free).max(min)).resizable(true)
            };
            table = table.column(col.at_least(min).clip(true));
        }
        let mut measured: Option<Vec<f32>> = None;

        let listed_ids = || listed.iter().filter_map(|s| s.id.as_ref());
        let n_sel = listed_ids()
            .filter(|id| self.selected.contains(*id))
            .count();
        let n_ids = listed_ids().count();
        let mut toggle_all = false;

        table
            .header(row_h, |mut h| {
                h.col(|ui| {
                    let mut all = n_ids > 0 && n_sel == n_ids;
                    let tip = if n_ids == 0 && !listed.is_empty() {
                        "Individual selection requires the game database; Export all exports every song"
                    } else {
                        "Select / deselect all listed songs"
                    };
                    toggle_all = ui
                        .add_enabled(
                            enabled && n_ids > 0,
                            egui::Checkbox::without_text(&mut all)
                                .indeterminate(n_sel > 0 && n_sel < n_ids),
                        )
                        .on_hover_text(tip)
                        .on_disabled_hover_text(tip)
                        .clicked();
                });
                let mut cols = vec![
                    ("ID", SortCol::Id),
                    ("Title", SortCol::Title),
                    ("Artist", SortCol::Artist),
                ];
                if show_genre {
                    cols.push(("Genre", SortCol::Genre));
                }
                if show_version {
                    cols.push(("Version", SortCol::Version));
                }
                for (label, col) in cols {
                    h.col(|ui| {
                        if sort_header(ui, label, col, sort) {
                            sort_clicked = Some(col);
                        }
                    });
                }
            })
            .body(|body| {
                measured = Some(body.widths().to_vec());
                body.rows(row_h, listed.len(), |mut row| {
                    let i = row.index();
                    let song = &listed[i];
                    let selectable = enabled && song.id.is_some();
                    let mut is_sel = song
                        .id
                        .as_ref()
                        .is_some_and(|id| self.selected.contains(id));
                    row.set_selected(is_sel);
                    let mut box_clicked = false;
                    row.col(|ui| {
                        box_clicked = ui
                            .add_enabled(selectable, egui::Checkbox::without_text(&mut is_sel))
                            .clicked();
                    });
                    row.col(|ui| {
                        cell(ui, song.id.as_deref().unwrap_or("–"));
                    });
                    row.col(|ui| {
                        let mut t = song.title.clone();
                        if song.tracks > 1 {
                            t.push_str(&format!("  ({} tracks)", song.tracks));
                        }
                        cell(ui, t);
                    });
                    row.col(|ui| {
                        cell(ui, &song.artist);
                    });
                    if show_genre {
                        row.col(|ui| {
                            cell(ui, &song.genre);
                        });
                    }
                    if show_version {
                        row.col(|ui| {
                            cell(ui, &song.version_label);
                        });
                    }
                    if selectable && (box_clicked || row.response().clicked()) {
                        clicked = Some(i);
                    }
                });
            });

        // Remember manual column resizes as shares of the table width.
        // Skip on refit frames so clamping to minimum widths during a window
        // resize doesn't overwrite the user's saved proportions.
        if !refit {
            if let Some(w) = measured.filter(|w| w.len() == n + 1) {
                let new: Vec<f32> = w[1..].iter().map(|w| w / free).collect();
                let changed = shares.iter().zip(&new).any(|(a, b)| (a - b).abs() > 0.002);
                if changed {
                    self.settings.col_shares.insert(layout, new);
                }
            }
        }

        if let Some(col) = sort_clicked {
            // Same column: flip direction. New column: ascending.
            if self.settings.sort_col == col {
                self.settings.sort_desc = !self.settings.sort_desc;
            } else {
                self.settings.sort_col = col;
                self.settings.sort_desc = false;
            }
            self.anchor = None; // row indices changed
        }

        if toggle_all {
            let ids: Vec<String> = listed_ids().cloned().collect();
            if n_sel == n_ids {
                for id in &ids {
                    self.selected.remove(id);
                }
            } else {
                self.selected.extend(ids);
            }
            self.anchor = None;
        }

        if let Some(i) = clicked {
            match (shift, self.anchor) {
                (true, Some(a)) => {
                    let (lo, hi) = if a <= i { (a, i) } else { (i, a) };
                    let lo = lo.min(listed.len().saturating_sub(1));
                    let hi = hi.min(listed.len().saturating_sub(1));
                    self.selected
                        .extend(listed[lo..=hi].iter().filter_map(|s| s.id.clone()));
                }
                _ => {
                    if let Some(id) = &listed[i].id {
                        if !self.selected.remove(id) {
                            self.selected.insert(id.clone());
                        }
                    }
                    self.anchor = Some(i);
                }
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let selected_n = self.selected.len();

        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                let cancelled = job.shared.cancel.is_cancelled();
                if ui
                    .add_enabled(!cancelled, egui::Button::new("Cancel"))
                    .clicked()
                {
                    job.shared.cancel.cancel();
                    job.shared
                        .push(Level::Warn, "cancelling — waiting for in-flight tracks");
                }
                ui.spinner();
            } else {
                let export_all_fallback = self
                    .catalog()
                    .is_some_and(|c| !c.has_ids && !c.songs.is_empty());
                let label = if export_all_fallback {
                    "  Export all  ".to_string()
                } else if selected_n == 0 {
                    "  Export  ".to_string()
                } else {
                    format!("  Export ({selected_n})  ")
                };
                if ui
                    .add_enabled(
                        self.catalog().is_some() && (export_all_fallback || selected_n > 0),
                        egui::Button::new(label),
                    )
                    .on_disabled_hover_text("Select songs to export")
                    .clicked()
                {
                    self.start(&ctx);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Open output folder").clicked() {
                    open_folder(Path::new(self.settings.output.trim()));
                }
                let cmd = self.export_argv();
                if ui
                    .add_enabled(cmd.is_ok(), egui::Button::new("Copy command"))
                    .on_hover_text("Copy the equivalent CLI invocation")
                    .clicked()
                {
                    if let Ok(argv) = cmd {
                        ctx.copy_text(quote_argv(&argv));
                    }
                }
            });
        });

        if let Some(err) = &self.form_error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }

        if let Some(job) = &self.job {
            let sh = &job.shared;
            if sh.planning.load(Ordering::Relaxed) {
                ui.label("Planning…");
            } else {
                let total = sh.total.load(Ordering::Relaxed);
                let done = sh.done.load(Ordering::Relaxed);
                let frac = if total == 0 {
                    0.0
                } else {
                    done as f32 / total as f32
                };
                let current = sh.current.lock().unwrap().clone();
                ui.add(
                    egui::ProgressBar::new(frac)
                        .text(format!("{done}/{total}  {current}"))
                        .desired_width(f32::INFINITY),
                );
            }
        }
    }

    fn log(&self, ui: &mut egui::Ui) {
        let Some(job) = self.job.as_ref().or(self.last.as_ref()) else {
            ui.weak("Export output appears here. \"Copy command\" gives the equivalent unigst (CLI) command.");
            return;
        };
        let log = job.shared.log.lock().unwrap();

        ui.horizontal(|ui| {
            ui.strong("Log");
            ui.weak(format!("({} lines)", log.len()));
            if ui.small_button("Copy").clicked() {
                let text: Vec<&str> = log.iter().map(|(_, l)| l.as_str()).collect();
                ui.ctx().copy_text(text.join("\n"));
            }
        });

        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        let warn = ui.visuals().warn_fg_color;
        let err = ui.visuals().error_fg_color;
        egui::ScrollArea::both()
            .id_salt("log")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, row_h, log.len(), |ui, range| {
                for (level, line) in &log[range] {
                    let text = egui::RichText::new(line).monospace();
                    let text = match level {
                        Level::Info => text,
                        Level::Warn => text.color(warn),
                        Level::Error => text.color(err),
                        Level::Summary => text.strong(),
                    };
                    ui.add(egui::Label::new(text).extend());
                }
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);
        self.poll_scan();

        egui::Panel::top("form_panel").show(ui, |ui| {
            ui.add_space(6.0);
            self.form(ui);
            ui.add_space(6.0);
        });
        egui::Panel::bottom("run_panel")
            .resizable(true)
            .default_size(220.0)
            .min_size(120.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                self.controls(ui);
                ui.separator();
                self.log(ui);
            });
        egui::CentralPanel::default().show(ui, |ui| self.songs(ui));

        // After the form so `editing_path` reflects this frame.
        self.maybe_rescan(&ctx);

        if self.running() || matches!(self.scan, ScanState::Scanning { .. }) {
            // Keep the spinner/progress ticking even if the worker is quiet.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}

// ---------------------------------------------------------------------------
// Helpers

/// Fonts.
///
/// Windows: use system fonts only (Segoe UI / Consolas / Segoe UI Symbol plus
/// Japanese, Korean and Chinese fonts). They match the OS look and let the
/// exe skip egui's bundled fonts. Elsewhere: egui's bundled fonts plus a CJK
/// system font as fallback.
///
/// Errors if no usable UI font exists (the window would render no text).
fn install_fonts(ctx: &egui::Context) -> Result<(), String> {
    use egui::{FontData, FontFamily};

    // Each group: first file that exists wins.
    #[cfg(windows)]
    let (ui_fonts, mono_fonts, fallbacks): (&[&str], &[&str], &[&[&str]]) = (
        &["segoeui.ttf", "tahoma.ttf", "arial.ttf"],
        &["consola.ttf", "lucon.ttf", "cour.ttf"],
        &[
            // Japanese first so shared kanji (and ☆ ♪ ～ …) get Japanese forms.
            &["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"],
            // Remaining symbols
            &["seguisym.ttf"],
            // Korean
            &["malgun.ttf", "gulim.ttc"],
            // Chinese (Simplified, Traditional)
            &["msyh.ttc", "msjh.ttc"],
        ],
    );
    #[cfg(not(windows))]
    let (ui_fonts, mono_fonts, fallbacks): (&[&str], &[&str], &[&[&str]]) = (
        &[],
        &[],
        &[&[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ]],
    );

    #[cfg(windows)]
    let font_dir = std::env::var_os("WINDIR")
        .map(|w| PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\Fonts"));
    #[cfg(not(windows))]
    let font_dir = PathBuf::from("/");

    let load = |files: &[&str]| -> Option<FontData> {
        files
            .iter()
            .find_map(|f| std::fs::read(font_dir.join(f)).ok())
            .map(FontData::from_owned)
    };

    // Without `default_fonts` (Windows builds) this starts empty.
    let mut defs = egui::FontDefinitions::default();
    let prepend =
        |defs: &mut egui::FontDefinitions, name: &str, data: FontData, families: &[FontFamily]| {
            defs.font_data.insert(name.into(), Arc::new(data));
            for fam in families {
                defs.families
                    .entry(fam.clone())
                    .or_default()
                    .insert(0, name.into());
            }
        };

    // Baseline metrics of each family's primary font (Windows only; elsewhere
    // the primary is egui's bundled font and fallbacks are left as is).
    let mut prop_metrics = None;
    let mut mono_metrics = None;

    if !ui_fonts.is_empty() {
        let Some(ui) = load(ui_fonts) else {
            return Err(format!(
                "no usable UI font found in {} (looked for {})",
                font_dir.display(),
                ui_fonts.join(", ")
            ));
        };
        prop_metrics = VMetrics::of(&ui);
        mono_metrics = prop_metrics;
        prepend(&mut defs, "ui", ui.clone(), &[FontFamily::Proportional]);
        // Monospace: a mono font if present, the UI font otherwise.
        prepend(&mut defs, "ui-mono-fallback", ui, &[FontFamily::Monospace]);
        if let Some(mono) = load(mono_fonts) {
            mono_metrics = VMetrics::of(&mono).or(mono_metrics);
            prepend(&mut defs, "mono", mono, &[FontFamily::Monospace]);
        }
    }

    for (i, files) in fallbacks.iter().enumerate() {
        let Some(data) = load(files) else {
            continue;
        };
        let fallback = VMetrics::of(&data);
        // One entry per family: the baseline correction depends on the
        // primary font it is mixed with.
        for (fam, primary) in [
            (FontFamily::Proportional, prop_metrics),
            (FontFamily::Monospace, mono_metrics),
        ] {
            let y_offset_factor = match (primary, fallback) {
                (Some(p), Some(f)) => baseline_offset(p, f),
                _ => 0.0,
            };
            let name = format!("fallback-{i}-{fam:?}");
            let tweaked = data.clone().tweak(egui::FontTweak {
                y_offset_factor,
                ..Default::default()
            });
            defs.font_data.insert(name.clone(), Arc::new(tweaked));
            defs.families.entry(fam).or_default().push(name);
        }
    }

    ctx.set_fonts(defs);
    Ok(())
}

/// Vertical font metrics in em units (descent is negative), as egui reads them.
#[derive(Debug, Clone, Copy)]
struct VMetrics {
    ascent: f32,
    descent: f32,
    leading: f32,
}

impl VMetrics {
    fn of(data: &egui::FontData) -> Option<Self> {
        use skrifa::MetadataProvider;
        let font = skrifa::FontRef::from_index(data.bytes(), data.index).ok()?;
        let m = font.metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        let upem = f32::from(m.units_per_em);
        (upem > 0.0).then(|| VMetrics {
            ascent: m.ascent / upem,
            descent: m.descent / upem,
            leading: m.leading / upem,
        })
    }

    fn height(self) -> f32 {
        self.ascent - self.descent + self.leading
    }
}

/// `FontTweak::y_offset_factor` that puts a fallback font's baseline on the
/// primary font's. egui centres a fallback glyph vertically in the line
/// instead (epaint `text_layout`: baseline at `face_ascent + (h - face_h)/2`),
/// so fonts with taller metrics, such as Yu Gothic next to Segoe UI, end up
/// shifted vertically.
fn baseline_offset(primary: VMetrics, fallback: VMetrics) -> f32 {
    (primary.ascent - fallback.ascent) + 0.5 * (fallback.height() - primary.height())
}

/// Case-insensitive sort key where digit runs compare by value
/// ("Artist 2" < "Artist 10"): each run is zero-padded to a fixed width.
fn natural_key(s: &str) -> String {
    const WIDTH: usize = 20;
    let mut out = String::with_capacity(s.len() + 8);
    let mut digits = String::new();
    let flush = |out: &mut String, digits: &mut String| {
        if !digits.is_empty() {
            let trimmed = digits.trim_start_matches('0');
            for _ in trimmed.len()..WIDTH {
                out.push('0');
            }
            out.push_str(trimmed);
            digits.clear();
        }
    };
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            flush(&mut out, &mut digits);
            out.push(c);
        }
    }
    flush(&mut out, &mut digits);
    out
}

/// Clickable column header with a sort-direction arrow when active.
/// Returns whether it was clicked.
fn sort_header(ui: &mut egui::Ui, label: &str, col: SortCol, sort: (SortCol, bool)) -> bool {
    let active = sort.0 == col;
    let r = ui
        .add(
            egui::Label::new(egui::RichText::new(label).strong())
                .selectable(false)
                .sense(egui::Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Sort");
    if active {
        // Painted rather than a glyph so it renders with any font:
        // ▲ ascending, ▼ descending.
        let size = ui.text_style_height(&egui::TextStyle::Body) * 0.45;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        let c = rect.center();
        let h = size * 0.5;
        let pts = if sort.1 {
            vec![
                c + egui::vec2(-h, -h * 0.6),
                c + egui::vec2(h, -h * 0.6),
                c + egui::vec2(0.0, h * 0.6),
            ]
        } else {
            vec![
                c + egui::vec2(-h, h * 0.6),
                c + egui::vec2(h, h * 0.6),
                c + egui::vec2(0.0, -h * 0.6),
            ]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            pts,
            ui.visuals().strong_text_color(),
            egui::Stroke::NONE,
        ));
    }
    r.clicked()
}

/// Table cell text. Not selectable: a selectable label would swallow the
/// click meant for the row.
fn cell(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).truncate().selectable(false));
}

fn format_label(f: &str) -> &'static str {
    match f {
        "mp3" => "MP3 (320k)",
        "flac" => "FLAC",
        _ => "Opus (192k VBR)",
    }
}

/// Text field + "Browse…" button. Returns the text field's response.
fn path_row(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    pick: impl FnOnce() -> Option<PathBuf>,
) -> egui::Response {
    ui.horizontal(|ui| {
        let browse_w = 80.0;
        let r = ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width((ui.available_width() - browse_w).max(120.0)),
        );
        if ui.button("Browse…").clicked() {
            if let Some(p) = pick() {
                *value = p.display().to_string();
            }
        }
        r
    })
    .inner
}

/// File dialog that opens at `current` (or its nearest existing ancestor).
fn dialog_in(current: &str) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new();
    let mut p = Path::new(current.trim());
    loop {
        if p.as_os_str().is_empty() {
            return dialog;
        }
        if p.is_dir() {
            return dialog.set_directory(p);
        }
        if p.is_file() {
            let d = dialog.set_directory(p.parent().unwrap_or(p));
            return match p.file_name() {
                Some(name) => d.set_file_name(name.to_string_lossy()),
                None => d,
            };
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return dialog,
        }
    }
}

fn open_folder(path: &Path) {
    let path = if path.as_os_str().is_empty() {
        Path::new("GST")
    } else {
        path
    };
    let _ = std::fs::create_dir_all(path);
    // Not canonicalize(): explorer.exe rejects \\?\ verbatim paths.
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{natural_key, quote_argv, Catalog, SongEntry, VersionFilter};

    fn song(id: &str, version: Option<&str>) -> SongEntry {
        SongEntry {
            id: Some(id.into()),
            title: id.into(),
            artist: String::new(),
            genre: String::new(),
            version: version.map(Into::into),
            version_label: version.unwrap_or("Unknown/Removed").into(),
            version_order: version.and_then(|v| v.parse().ok()).unwrap_or(u32::MAX),
            tracks: 1,
        }
    }

    #[test]
    fn unknown_songs_are_not_in_all_versions() {
        let cat = Catalog::new(
            vec![
                song("1", Some("33")),
                song("2", Some("34")),
                song("3", None),
                song("4", None),
            ],
            vec![],
        );
        assert_eq!((cat.known, cat.unknown, cat.all_count()), (2, 2, 2));
        let all: Vec<_> = cat
            .songs
            .iter()
            .filter(|s| VersionFilter::All.matches(s, cat.unknown_in_all()))
            .map(|s| s.id.clone().unwrap())
            .collect();
        assert_eq!(all, ["1", "2"]);
        let unknown = cat
            .songs
            .iter()
            .filter(|s| VersionFilter::Unknown.matches(s, cat.unknown_in_all()))
            .count();
        assert_eq!(unknown, 2);
        // Still offered as its own version entry.
        assert!(cat
            .versions
            .iter()
            .any(|v| v.filter == VersionFilter::Unknown && v.count == 2));
    }

    #[test]
    fn all_versions_keeps_songs_when_none_have_a_version() {
        // e.g. pop'n music db unreadable: hiding everything would be useless.
        let cat = Catalog::new(vec![song("1", None), song("2", None)], vec![]);
        assert!(cat.unknown_in_all());
        assert_eq!(cat.all_count(), 2);
    }

    #[test]
    fn natural_ordering() {
        let mut v = vec![
            "Artist 10",
            "artist 2",
            "Artist 1",
            "Beta",
            "alpha",
            "Op.02",
            "Op.1",
        ];
        v.sort_by_key(|s| natural_key(s));
        assert_eq!(
            v,
            [
                "alpha",
                "Artist 1",
                "artist 2",
                "Artist 10",
                "Beta",
                "Op.1",
                "Op.02"
            ]
        );
        // Non-ASCII text is kept (lowercased) as is.
        assert_eq!(natural_key("ネオン"), "ネオン");
    }

    #[test]
    fn quote_argv_handles_spaces_quotes_metachars_and_trailing_backslashes() {
        let argv: Vec<OsString> = [
            "unigst",
            "sdvx",
            r"--input=C:\My Games\SDVX\",
            r#"--output=C:\Out\say "hi""#,
            r"--output=C:\Rhythm&Music(1)",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        assert_eq!(
            quote_argv(&argv),
            r#"unigst sdvx "--input=C:\My Games\SDVX\\" "--output=C:\Out\say \"hi\"" "--output=C:\Rhythm&Music(1)""#
        );
    }

    #[test]
    fn fallback_catalog_without_ids_marks_has_ids_false() {
        let cat = Catalog::new(
            vec![SongEntry {
                id: None,
                title: "track01".into(),
                artist: String::new(),
                genre: String::new(),
                version: None,
                version_label: "Unknown/Removed".into(),
                version_order: u32::MAX,
                tracks: 1,
            }],
            vec![],
        );
        assert!(!cat.has_ids);
        assert!(cat.unknown_in_all());
    }
}
