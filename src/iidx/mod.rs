mod chart;
mod mix;
mod musicdata;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::IidxArgs;
use crate::common::audio::{normalize, wav24_bytes, Pcm};
use crate::common::ffmpeg::{self, EncodeInput, EncodeJob};
use crate::common::fs;
use crate::common::formats::{archive, decode, ifs};
use crate::common::report::{self, CancelToken, Collector, PlannedTrack, ProgressSink, RunReport};
use crate::common::sanitize;
use crate::common::tags::TrackTags;

use musicdata::SongMeta;

enum Source {
    Dir(PathBuf),

    Ifs(PathBuf),
}

struct SongFiles {
    song_id: u32,

    name: String,
    source: Source,
}

const CHART_PREFERENCE: &[i32] = &[0, 1, 2, 3, 4, 6, 7, 8, 10];

struct Resolved {
    chart: Vec<u8>,
    audio: Vec<u8>,
    audio_name: String,
}

struct SongJob {
    files: SongFiles,
    meta: Option<SongMeta>,

    out: PathBuf,
}

pub struct Plan {
    jobs: Vec<SongJob>,

    pub warnings: Vec<String>,
}

impl Plan {
    fn source_counts(&self) -> (usize, usize) {
        let ifs = self
            .jobs
            .iter()
            .filter(|j| matches!(j.files.source, Source::Ifs(_)))
            .count();
        (ifs, self.jobs.len() - ifs)
    }
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
                id: job.files.song_id.to_string(),
                title: job
                    .meta
                    .as_ref()
                    .map(|m| m.title.clone())
                    .unwrap_or_else(|| format!("{:05}", job.files.song_id)),
                artist: job.meta.as_ref().map(|m| m.artist.clone()).unwrap_or_default(),
                output: job.out.clone(),
            })
            .collect()
    }

    fn dry_run_summary(&self) -> String {
        let (ifs, extracted) = self.source_counts();
        format!(
            "\n{} song(s) selected ({ifs} in .ifs, {extracted} extracted)",
            self.len()
        )
    }

    fn detail_lines(&self) -> Vec<String> {
        self.jobs
            .iter()
            .map(|job| {
                let src = match job.files.source {
                    Source::Ifs(_) => "ifs",
                    Source::Dir(_) => "dir",
                };
                let (cid, nevents, audio) = match resolve(&job.files, false) {
                    Ok(r) => {
                        let (cid, nevents) = chart::parse_chart(&r.chart, CHART_PREFERENCE)
                            .map(|(id, ev)| (id, ev.len()))
                            .unwrap_or((-1, 0));
                        (cid, nevents, r.audio_name)
                    }
                    Err(_) => (-1, 0, "<error>".into()),
                };
                let title = job.meta.as_ref().map(|m| m.title.as_str()).unwrap_or("<no metadata>");
                let artist = job.meta.as_ref().map(|m| m.artist.as_str()).unwrap_or("");
                format!(
                    "{:>5} [{src}] chart={cid} events={nevents} audio={audio} \"{title}\" - {artist}",
                    job.files.song_id
                )
            })
            .collect()
    }
}

pub fn plan(args: &IidxArgs) -> Result<Plan> {
    let common = &args.common;

    if !common.input.is_dir() {
        bail!("input directory not found: {}", common.input.display());
    }

    let sound_root = find_sound_root(&common.input);
    let (meta, mut warnings) = load_music_data(&common.input);

    let mut songs = discover_songs(&sound_root);
    if songs.is_empty() {
        bail!(
            "no IIDX songs found under {} — is this the game's contents directory?",
            sound_root.display()
        );
    }
    songs.sort_by_key(|s| s.song_id);

    songs.retain(|s| {
        if !args.id.is_empty() && !args.id.contains(&s.song_id) {
            return false;
        }
        if let Some(v) = args.version {
            match meta.get(&s.song_id) {
                Some(m) => m.game_version == v,
                None => false,
            }
        } else {
            true
        }
    });

    for id in &args.id {
        if !songs.iter().any(|s| s.song_id == *id) {
            warnings.push(format!("--id {id}: no song matched the selection"));
        }
    }

    let mut jobs: Vec<SongJob> = songs
        .into_iter()
        .map(|files| {
            let meta = meta.get(&files.song_id).cloned();
            let out = output_path(&common.output, &files, meta.as_ref(), common.format.ext());
            SongJob { files, meta, out }
        })
        .collect();

    let mut seen = HashSet::new();
    jobs.retain(|job| {
        if seen.insert(job.out.clone()) {
            true
        } else {
            warnings.push(format!(
                "{}: duplicate output {}, skipped",
                job.files.song_id,
                job.out.display()
            ));
            false
        }
    });

    Ok(Plan { jobs, warnings })
}

pub fn execute(
    args: &IidxArgs,
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
                collector.fail(format!("{}: {err:#}", job.files.song_id), sink);
            }
        },
    )
}

fn find_sound_root(input: &Path) -> PathBuf {
    fs::first_existing_dir([input.join("data").join("sound"), input.join("sound")])
        .unwrap_or_else(|| input.to_path_buf())
}

fn load_music_data(input: &Path) -> (HashMap<u32, SongMeta>, Vec<String>) {
    let info = input.join("data").join("info");

    let mut numbered: Vec<(u32, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&info) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                if let Some(n) = p.file_name().and_then(|n| n.to_str()).and_then(|s| s.parse().ok()) {
                    let bin = p.join("music_data.bin");
                    if bin.is_file() {
                        numbered.push((n, bin));
                    }
                }
            }
        }
    }
    numbered.sort_by_key(|(n, _)| *n);

    let mut paths: Vec<PathBuf> = Vec::new();
    for flat in [
        info.join("music_data.bin"),
        input.join("data").join("music_data.bin"),
        input.join("music_data.bin"),
    ] {
        if flat.is_file() {
            paths.push(flat);
        }
    }
    paths.extend(numbered.into_iter().map(|(_, p)| p));

    let mut merged: HashMap<u32, SongMeta> = HashMap::new();
    let mut warnings = Vec::new();
    for path in &paths {
        match musicdata::parse_file(path) {
            Ok(map) => merged.extend(map),
            Err(err) => warnings.push(format!("{}: {err:#}", path.display())),
        }
    }
    (merged, warnings)
}

fn discover_songs(root: &Path) -> Vec<SongFiles> {
    let mut found: HashMap<u32, SongFiles> = HashMap::new();
    collect(root, &mut found);
    let mut out: Vec<SongFiles> = found.into_values().collect();
    out.sort_by_key(|s| s.song_id);
    out
}

fn collect(dir: &Path, out: &mut HashMap<u32, SongFiles>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };

        if path.is_dir() {
            if let Ok(song_id) = name.parse::<u32>() {
                if path.join(format!("{name}.1")).is_file() {
                    out.insert(
                        song_id,
                        SongFiles {
                            song_id,
                            name: name.clone(),
                            source: Source::Dir(path.clone()),
                        },
                    );
                }
            }
            collect(&path, out);
        } else if let Some(stem) = name.strip_suffix(".ifs") {
            if let Ok(song_id) = stem.parse::<u32>() {
                if !ifs::is_real_ifs(&path) {
                    continue;
                }
                out.entry(song_id).or_insert_with(|| SongFiles {
                    song_id,
                    name: stem.to_string(),
                    source: Source::Ifs(path.clone()),
                });
            }
        }
    }
}

fn is_gameplay_audio(name: &str) -> bool {
    let stem = match name.strip_suffix(".2dx").or_else(|| name.strip_suffix(".s3p")) {
        Some(s) => s,
        None => return false,
    };
    !stem.ends_with("_pre") && !stem.ends_with("_fx")
}

const AUDIO_HEADER_PEEK: usize = 0x18;

fn resolve(song: &SongFiles, load_audio: bool) -> Result<Resolved> {
    match &song.source {
        Source::Dir(dir) => {
            let chart = std::fs::read(dir.join(format!("{}.1", song.name)))
                .with_context(|| format!("reading chart for {}", song.name))?;

            let mut candidates: Vec<PathBuf> = Vec::new();
            for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
                let p = entry.path();
                if p.file_name().and_then(|n| n.to_str()).is_some_and(is_gameplay_audio) {
                    candidates.push(p);
                }
            }
            let best = select_best(&candidates, |p| {
                (
                    p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
                    peek_sample_count(&read_file_prefix(p, AUDIO_HEADER_PEEK)),
                )
            });
            let Some(best) = best else {
                bail!("no audio archive for {}", song.name);
            };
            let audio = if load_audio {
                std::fs::read(&candidates[best])?
            } else {
                Vec::new()
            };
            Ok(Resolved {
                chart,
                audio,
                audio_name: fs::file_name(&candidates[best]),
            })
        }
        Source::Ifs(path) => {
            let idx = ifs::index(path)?;
            let chart = idx
                .read(&format!("{}.1", song.name))
                .with_context(|| format!("{}.1 missing in {}", song.name, path.display()))?;

            let candidates: Vec<String> =
                idx.names().filter(|n| is_gameplay_audio(n)).map(String::from).collect();
            let best = select_best(&candidates, |n| {
                let head = idx.read_prefix(n, AUDIO_HEADER_PEEK).unwrap_or_default();
                (n.clone(), peek_sample_count(&head))
            });
            let Some(best) = best else {
                bail!("no audio archive in {}", path.display());
            };
            let name = candidates[best].clone();
            let audio = if load_audio {
                idx.read(&name).with_context(|| format!("reading {name} from {}", path.display()))?
            } else {
                Vec::new()
            };
            Ok(Resolved {
                chart,
                audio,
                audio_name: name,
            })
        }
    }
}

fn peek_sample_count(head: &[u8]) -> usize {
    archive::sample_count(head).unwrap_or(0)
}

fn read_file_prefix(path: &Path, max: usize) -> Vec<u8> {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buf = Vec::with_capacity(max);
    let _ = f.take(max as u64).read_to_end(&mut buf);
    buf
}

fn select_best<T>(items: &[T], key: impl Fn(&T) -> (String, usize)) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .map(|(i, it)| (i, key(it)))
        .max_by(|(_, (na, ca)), (_, (nb, cb))| ca.cmp(cb).then_with(|| nb.cmp(na)))
        .map(|(i, _)| i)
}

fn process_song(
    job: &SongJob,
    ffmpeg: &Path,
    args: &IidxArgs,
    collector: &Collector,
    sink: &dyn ProgressSink,
) -> Result<()> {
    let common = &args.common;
    let meta = job.meta.as_ref();
    let out = &job.out;

    sink.started(
        &meta
            .map(|m| m.title.clone())
            .unwrap_or_else(|| job.files.name.clone()),
    );

    if out.exists() && !common.overwrite {
        collector.skip(format!("skip existing {}", out.display()), sink);
        return Ok(());
    }

    let resolved = match resolve(&job.files, true) {
        Ok(r) => r,
        Err(err) => {
            collector.warn(
                format!("{}: unreadable, skipped ({err:#})", job.files.song_id),
                sink,
            );
            return Ok(());
        }
    };

    let Some((_chart_id, events)) = chart::parse_chart(&resolved.chart, CHART_PREFERENCE) else {
        collector.warn(format!("{}: no usable chart", job.files.song_id), sink);
        return Ok(());
    };

    let referenced: HashSet<usize> = events
        .iter()
        .filter(|e| e.keysound_id >= 0)
        .map(|e| e.keysound_id as usize)
        .collect();
    let blobs = archive::read_bytes(&resolved.audio)?;
    let mut failed = 0usize;
    let pcm_cache: Vec<Pcm> = blobs
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if !referenced.contains(&i) {
                return Pcm::default();
            }
            decode::decode_blob(b, ffmpeg).unwrap_or_else(|_| {
                failed += 1;
                Pcm::default()
            })
        })
        .collect();
    if failed > 0 {
        collector.warn(
            format!(
                "{}: {failed} keysound(s) failed to decode — mixed as silence",
                job.files.song_id
            ),
            sink,
        );
    }

    let mut mixed = mix::build_mix(&events, &pcm_cache, default_volume(meta))?;
    let peak = normalize(&mut mixed);
    if peak > 1.0 {
        sink.logged(&format!(
            "{}: mix peaked at {peak:.2}x full scale — normalized",
            job.files.song_id
        ));
    }
    let wav = wav24_bytes(&mixed)?;
    drop(mixed);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    let tags = build_tags(&job.files, meta);
    let encode_job = EncodeJob {
        input: EncodeInput::WavBytes(&wav),
        output: out,
        format: common.format,
        tags: &tags,
        cover: None,
    };
    ffmpeg::encode(ffmpeg, &encode_job)?;
    collector.ok();
    Ok(())
}

fn default_volume(meta: Option<&SongMeta>) -> f32 {
    match meta {
        Some(m) => ((m.volume as f64 / 127.0 * 100.0).round() / 100.0) as f32,
        None => 0.85,
    }
}

fn album_name(meta: Option<&SongMeta>) -> String {
    match meta.and_then(|m| musicdata::style(m.game_version).map(|s| (m.game_version, s))) {
        Some((ver, (name, _year))) if ver > 10 => format!("beatmania IIDX {ver} {name} GST"),
        Some((_, (name, _year))) => format!("beatmania IIDX {name} GST"),
        None => "beatmania IIDX GST".to_string(),
    }
}

fn output_path(out_root: &Path, song: &SongFiles, meta: Option<&SongMeta>, ext: &str) -> PathBuf {
    let stem = match meta {
        Some(m) => format!("{} - {}", sanitize::clean(&m.artist), sanitize::clean(&m.title)),
        None => format!("{:05}", song.song_id),
    };
    sanitize::output_path(out_root, &album_name(meta), &stem, ext)
}

fn build_tags(song: &SongFiles, meta: Option<&SongMeta>) -> TrackTags {
    let Some(m) = meta else {
        return TrackTags {
            title: format!("{:05}", song.song_id),
            ..Default::default()
        };
    };

    let comment = if !m.title_ascii.is_empty() && m.title_ascii != m.title {
        Some(m.title_ascii.clone())
    } else {
        None
    };

    let (album, date) = match musicdata::style(m.game_version) {
        Some((_, year)) => (album_name(meta), year.to_string()),
        None => (album_name(meta), String::new()),
    };

    TrackTags {
        title: m.title.clone(),
        artist: m.artist.clone(),
        album,
        album_artist: "Various Artists".to_string(),
        genre: m.genre.clone(),
        track: m.song_id.to_string(),
        disc: m.game_version.to_string(),
        date,
        comment,
        ..Default::default()
    }
}
