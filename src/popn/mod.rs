mod archive;
mod chart;
pub mod db;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::PopnArgs;
use crate::common::audio::{normalize, wav24_bytes, Pcm};
use crate::common::ffmpeg::{self, EncodeInput, EncodeJob};
use crate::common::formats::{archive::Blob, decode, ifs};
use crate::common::fs;
use crate::common::report::{
    self, CancelToken, Collector, PlannedTrack, ProgressSink, RunReport, SongEntry,
};
use crate::common::sanitize;
use crate::common::tags::TrackTags;

use chart::PlayEvent;
use db::PopnSong;

struct SongJob {
    id: Option<u32>,
    title: String,
    artist: String,
    genre: String,
    version: u32,

    ifs_paths: Vec<PathBuf>,
    out: PathBuf,
}

pub struct Plan {
    jobs: Vec<SongJob>,

    pub warnings: Vec<String>,
}

impl report::Plan for Plan {
    fn len(&self) -> usize {
        self.jobs.len()
    }

    fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn tracks(&self) -> Vec<PlannedTrack> {
        self.jobs
            .iter()
            .map(|job| PlannedTrack {
                id: job.id.map(|i| i.to_string()).unwrap_or_default(),
                title: job.title.clone(),
                artist: job.artist.clone(),
                output: job.out.clone(),
            })
            .collect()
    }

    fn songs(&self) -> Vec<SongEntry> {
        self.jobs
            .iter()
            .map(|job| {
                // Without the music db (no popn.dll) there are no ids or versions.
                let known = job.id.is_some();
                SongEntry {
                    id: job.id.map(|i| i.to_string()),
                    title: job.title.clone(),
                    artist: job.artist.clone(),
                    genre: job.genre.clone(),
                    version: known.then(|| job.version.to_string()),
                    version_label: if known {
                        db::style(job.version)
                            .map(|(name, _)| name.to_string())
                            .unwrap_or_else(|| job.version.to_string())
                    } else {
                        "Unknown/Removed".into()
                    },
                    version_order: if known { job.version } else { u32::MAX },
                    tracks: 1,
                }
            })
            .collect()
    }

    fn detail_lines(&self) -> Vec<String> {
        self.jobs
            .iter()
            .map(|job| {
                let (chart_sfx, nevents, source) = match resolve(&job.ifs_paths) {
                    Ok(r) => (r.chart_suffix, r.events.len(), fs::file_name(&r.ifs_path)),
                    Err(_) => ("-".into(), 0, "<error>".into()),
                };
                let id = job.id.map(|i| i.to_string()).unwrap_or_else(|| "-".into());
                format!(
                    "{id:>5} [{source}] chart={chart_sfx} events={nevents} \"{}\" - {}",
                    job.title, job.artist
                )
            })
            .collect()
    }

    fn dry_run_summary(&self) -> String {
        format!(
            "\n{} song(s) selected ({} planning warning(s))",
            self.len(),
            self.warnings.len()
        )
    }
}

struct Resolved {
    ifs_path: PathBuf,
    chart_suffix: String,
    events: Vec<PlayEvent>,
    keysounds: archive::Keysounds,
}

pub fn plan(args: &PopnArgs) -> Result<Plan> {
    let common = &args.common;

    if !common.input.is_dir() {
        bail!("input directory not found: {}", common.input.display());
    }
    let sd = find_sd_root(&common.input).with_context(|| {
        format!(
            "no sd/ song directory under {} — is this the game's install directory?",
            common.input.display()
        )
    })?;

    let mut warnings = Vec::new();
    let songs = load_db(args, &mut warnings);

    let mut jobs = match songs {
        Some(songs) => db_jobs(args, songs, &sd, &mut warnings),
        None => {
            if !args.id.is_empty() || args.version.is_some() {
                let reason = warnings
                    .first()
                    .map(String::as_str)
                    .unwrap_or("music db unavailable");
                bail!("cannot filter by --id or --version without a readable pop'n DLL ({reason})");
            }
            fallback_jobs(args, &sd)
        }
    };
    if jobs.is_empty() {
        bail!(
            "no pop'n music songs matched the selection under {}",
            sd.display()
        );
    }
    jobs.sort_by_key(|j| j.id.unwrap_or(u32::MAX));

    for id in &args.id {
        if !jobs.iter().any(|j| j.id == Some(*id)) {
            warnings.push(format!("--id {id}: no song matched the selection"));
        }
    }

    let mut seen = HashSet::new();
    jobs.retain(|job| {
        if seen.insert(job.out.clone()) {
            true
        } else {
            warnings.push(format!(
                "{}: duplicate output {}, skipped",
                job.title,
                job.out.display()
            ));
            false
        }
    });

    Ok(Plan { jobs, warnings })
}

pub fn execute(
    args: &PopnArgs,
    plan: &Plan,
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<RunReport> {
    let ffmpeg = ffmpeg::locate()?;
    report::run_jobs(
        &plan.jobs,
        args.common.thread_count(),
        &plan.warnings,
        sink,
        cancel,
        |job, collector, sink| {
            if let Err(err) = process_song(job, &ffmpeg, args, collector, sink) {
                collector.fail(format!("{}: {err:#}", job.title), sink);
            }
        },
    )
}

/// Main game DLL name pattern (for messages); see [`is_game_dll_name`].
pub const GAME_DLL: &str = "popn.dll / popn<N>.dll";

/// `popn.dll` or `popn` + digits (`popn22.dll`). Deliberately not `popn*.dll`:
/// mods such as `popnhax.dll` sit next to the game DLL and hold no music db.
fn is_game_dll_name(lowercase: &str) -> bool {
    lowercase
        .strip_prefix("popn")
        .and_then(|rest| rest.strip_suffix(".dll"))
        .is_some_and(|mid| mid.bytes().all(|b| b.is_ascii_digit()))
}

/// The game DLL (which holds the music db) in `modules/` or the root.
fn find_game_dll(input: &Path) -> Option<PathBuf> {
    fs::find_game_dll_by(input, is_game_dll_name)
}

/// Whether `input` is a pop'n music install folder: the game DLL and the sd/
/// song folder must both be present.
pub fn looks_like(input: &Path) -> bool {
    find_game_dll(input).is_some() && find_sd_root(input).is_some()
}

fn find_sd_root(input: &Path) -> Option<PathBuf> {
    fs::first_existing_dir([
        input.join("plain_data").join("sd"),
        input.join("data").join("sd"),
        input.join("sd"),
    ])
}

fn load_db(args: &PopnArgs, warnings: &mut Vec<String>) -> Option<Vec<PopnSong>> {
    let input = &args.common.input;
    let found = match &args.dll {
        Some(p) => Some(input.join(p)).filter(|p| p.is_file()),
        None => find_game_dll(input),
    };
    let Some(dll) = found else {
        let looked = match &args.dll {
            Some(p) => input.join(p).display().to_string(),
            None => format!(
                "{GAME_DLL} in {} and {}",
                input.join("modules").display(),
                input.display()
            ),
        };
        warnings.push(format!(
            "game DLL not found (looked for {looked}) — no titles/artists, filenames used instead"
        ));
        return None;
    };
    match db::parse_dll(&dll) {
        Ok(songs) => Some(songs),
        Err(err) => {
            warnings.push(format!(
                "{}: music db parse failed ({err:#}) — no titles/artists, filenames used instead",
                dll.display()
            ));
            None
        }
    }
}

fn db_jobs(
    args: &PopnArgs,
    songs: Vec<PopnSong>,
    sd: &Path,
    warnings: &mut Vec<String>,
) -> Vec<SongJob> {
    let ext = args.common.format.ext();
    songs
        .into_iter()
        .filter(|s| args.id.is_empty() || args.id.contains(&s.id))
        .filter(|s| args.version.map(|v| s.version == v).unwrap_or(true))
        .filter_map(|s| {
            let ifs_paths: Vec<PathBuf> = s
                .ifs
                .iter()
                .map(|(folder, name)| sd.join(folder).join(format!("{name}.ifs")))
                .filter(|p| ifs::is_real_ifs(p))
                .collect();
            if ifs_paths.is_empty() {
                if args.id.contains(&s.id) {
                    warnings.push(format!(
                        "--id {}: song data not on disk ({})",
                        s.id, s.title
                    ));
                }
                return None;
            }
            let out = output_path(&args.common.output, &s, ext);
            Some(SongJob {
                id: Some(s.id),
                title: s.title,
                artist: s.artist,
                genre: s.genre,
                version: s.version,
                ifs_paths,
                out,
            })
        })
        .collect()
}

fn fallback_jobs(args: &PopnArgs, sd: &Path) -> Vec<SongJob> {
    let ext = args.common.format.ext();
    let mut jobs = Vec::new();
    let Ok(folders) = std::fs::read_dir(sd) else {
        return jobs;
    };
    for folder in folders.filter_map(|e| e.ok()) {
        let dir = folder.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Some(stem) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".ifs"))
            else {
                continue;
            };
            if !ifs::is_real_ifs(&path) {
                continue;
            }
            let out = sanitize::output_path(
                &args.common.output,
                &album_name(0),
                &sanitize::clean(stem),
                ext,
            );
            jobs.push(SongJob {
                id: None,
                title: stem.to_string(),
                artist: String::new(),
                genre: String::new(),
                version: 0,
                ifs_paths: vec![path],
                out,
            });
        }
    }
    jobs
}

fn resolve(ifs_paths: &[PathBuf]) -> Result<Resolved> {
    let mut last_err = anyhow::anyhow!("no .ifs candidates");
    for path in ifs_paths {
        match resolve_one(path) {
            Ok(r) => return Ok(r),
            Err(err) => last_err = err.context(format!("in {}", path.display())),
        }
    }
    Err(last_err)
}

fn resolve_one(path: &Path) -> Result<Resolved> {
    let idx = ifs::index(path)?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("bad .ifs filename")?;

    let mut chosen = None;
    for sfx in chart::CHART_PREFERENCE {
        let name = format!("{stem}_{sfx}.bin");
        if let Some(bytes) = idx.read(&name) {
            if let Some(events) = chart::parse(&bytes) {
                if !events.is_empty() {
                    chosen = Some((sfx.to_string(), events));
                    break;
                }
            }
        }
    }
    let (chart_suffix, events) = chosen.context("no usable chart")?;

    let audio_name = format!("{stem}.2dx");
    let audio = match idx.read(&audio_name) {
        Some(bytes) => bytes,
        None => {
            let name = idx
                .names()
                .find(|n| n.ends_with(".2dx") && !n.ends_with("_pre.2dx"))
                .map(String::from)
                .context("no keysound container")?;
            idx.read(&name).context("reading keysound container")?
        }
    };
    let keysounds = archive::read(&audio)?;

    Ok(Resolved {
        ifs_path: path.to_path_buf(),
        chart_suffix,
        events,
        keysounds,
    })
}

const RATE: i64 = 44_100;

fn build_mix(
    events: &[PlayEvent],
    keysounds: &archive::Keysounds,
    pcm: &[Pcm],
) -> Result<Vec<f32>> {
    let mut max_end: i64 = 0;
    let placements: Vec<(i64, usize)> = events
        .iter()
        .filter_map(|e| {
            let entry = keysounds.entry_for(e.keysound)?;
            let frames = pcm.get(entry)?.frames();
            if frames == 0 {
                return None;
            }
            let start = e.offset_ms as i64 * RATE / 1000;
            max_end = max_end.max(start + frames as i64);
            Some((start, entry))
        })
        .collect();

    if max_end <= 0 {
        max_end = 1;
    }
    if max_end > (i32::MAX / 2 - 1) as i64 {
        bail!("rendered mix exceeds maximum buffer size");
    }

    let mut mixbuf = vec![0.0f32; max_end as usize * 2];
    for (start, entry) in placements {
        let samples = &pcm[entry].samples;
        let base = start as usize * 2;
        for (i, &s) in samples.iter().enumerate() {
            if let Some(slot) = mixbuf.get_mut(base + i) {
                *slot += s;
            }
        }
    }
    Ok(mixbuf)
}

fn process_song(
    job: &SongJob,
    ffmpeg: &Path,
    args: &PopnArgs,
    collector: &Collector,
    sink: &dyn ProgressSink,
) -> Result<()> {
    let common = &args.common;
    sink.started(&job.title);

    if job.out.exists() && !common.overwrite {
        collector.skip(format!("skip existing {}", job.out.display()), sink);
        return Ok(());
    }

    let resolved = match resolve(&job.ifs_paths) {
        Ok(r) => r,
        Err(err) => {
            collector.warn(
                format!("{}: unreadable, skipped ({err:#})", job.title),
                sink,
            );
            return Ok(());
        }
    };

    let referenced: HashSet<usize> = resolved
        .events
        .iter()
        .filter_map(|e| resolved.keysounds.entry_for(e.keysound))
        .collect();
    let mut failed = 0usize;
    let pcm: Vec<Pcm> = resolved
        .keysounds
        .wavs
        .iter()
        .enumerate()
        .map(|(i, bytes)| {
            if !referenced.contains(&i) {
                return Pcm::default();
            }
            decode::decode_blob(&Blob::Wav(bytes.clone()), ffmpeg).unwrap_or_else(|_| {
                failed += 1;
                Pcm::default()
            })
        })
        .collect();
    if failed > 0 {
        collector.warn(
            format!(
                "{}: {failed} keysound(s) failed to decode — mixed as silence",
                job.title
            ),
            sink,
        );
    }

    let mut mixed = build_mix(&resolved.events, &resolved.keysounds, &pcm)?;
    drop(pcm);
    let peak = normalize(&mut mixed);
    if peak > 1.0 {
        sink.logged(&format!(
            "{}: mix peaked at {peak:.2}x full scale — normalized",
            job.title
        ));
    }
    let wav = wav24_bytes(&mixed)?;
    drop(mixed);

    if let Some(parent) = job.out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    let tags = build_tags(job);
    let encode_job = EncodeJob {
        input: EncodeInput::WavBytes(&wav),
        output: &job.out,
        format: common.format,
        tags: &tags,
        cover: None,
    };
    ffmpeg::encode(ffmpeg, &encode_job)?;
    collector.ok();
    Ok(())
}

fn album_name(version: u32) -> String {
    match db::style(version) {
        Some(("", _)) => "pop'n music GST".to_string(),
        Some((name, _)) => format!("pop'n music {name} GST"),
        None if version > 0 => format!("pop'n music {version} GST"),
        None => "pop'n music GST".to_string(),
    }
}

fn output_path(out_root: &Path, song: &PopnSong, ext: &str) -> PathBuf {
    let stem = if song.artist.is_empty() {
        sanitize::clean(&song.title)
    } else {
        format!(
            "{} - {}",
            sanitize::clean(&song.artist),
            sanitize::clean(&song.title)
        )
    };
    sanitize::output_path(out_root, &album_name(song.version), &stem, ext)
}

fn build_tags(job: &SongJob) -> TrackTags {
    let date = db::style(job.version)
        .map(|(_, year)| year.to_string())
        .unwrap_or_default();
    TrackTags {
        title: job.title.clone(),
        artist: job.artist.clone(),
        album: album_name(job.version),
        album_artist: "Various Artists".to_string(),
        genre: job.genre.clone(),
        track: job.id.map(|i| i.to_string()).unwrap_or_default(),
        disc: if job.version > 0 {
            job.version.to_string()
        } else {
            String::new()
        },
        date,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::is_game_dll_name;

    #[test]
    fn game_dll_names() {
        assert!(is_game_dll_name("popn.dll"));
        assert!(is_game_dll_name("popn19.dll"));
        assert!(is_game_dll_name("popn22.dll"));
        assert!(!is_game_dll_name("popnhax.dll"));
        assert!(!is_game_dll_name("popn22_hook.dll"));
        assert!(!is_game_dll_name("popn.exe"));
        assert!(!is_game_dll_name("xpopn.dll"));
    }
}
