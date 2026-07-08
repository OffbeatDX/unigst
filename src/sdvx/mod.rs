mod db;
mod discover;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cli::SdvxArgs;
use crate::common::ffmpeg::{self, EncodeInput, EncodeJob};
use crate::common::report::{self, CancelToken, Collector, PlannedTrack, ProgressSink, RunReport};
use crate::common::sanitize;
use crate::common::tags::TrackTags;

use db::{version_name, Filters, Song, TOTAL_VERSIONS};
use discover::AudioVariant;

struct TrackJob {
    song: Song,
    variant: AudioVariant,

    multi: bool,

    out: PathBuf,
}

pub struct Plan {
    jobs: Vec<TrackJob>,

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
                id: job.song.song_id.clone(),
                title: display_title(job),
                artist: job.song.artist.clone(),
                output: job.out.clone(),
            })
            .collect()
    }

    fn dry_run_summary(&self) -> String {
        format!(
            "\n{} track(s) across the selection ({} planning warning(s))",
            self.len(),
            self.warnings.len()
        )
    }

    fn nothing_to_do(&self) -> bool {
        self.jobs.is_empty() && self.warnings.is_empty()
    }
}

pub fn plan(args: &SdvxArgs) -> Result<Plan> {
    let common = &args.common;
    let db_path = common
        .input
        .join("data")
        .join("others")
        .join("music_db.xml");
    let music_dir = common.input.join("data").join("music");
    let dummy_jacket = {
        let p = common
            .input
            .join("data")
            .join("graphics")
            .join("jk_dummy_b.png");
        p.exists().then_some(p)
    };

    let filters = Filters {
        version: args.version.as_deref(),
        ids: &args.id,
    };
    let songs = db::parse_music_db(&db_path, &filters)
        .with_context(|| format!("parsing {}", db_path.display()))?;

    let mut jobs: Vec<TrackJob> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for id in &args.id {
        if !songs.iter().any(|s| db::id_matches(id, &s.song_id)) {
            warnings.push(format!("--id {id}: no song matched the selection"));
        }
    }

    for song in songs {
        match discover::get_variants(&song, &music_dir, dummy_jacket.as_deref()) {
            Ok(variants) => {
                let multi = variants.len() > 1;
                for variant in variants {
                    let out = output_path(
                        &common.output,
                        args.version.as_deref(),
                        &song,
                        &variant,
                        multi,
                        common.format.ext(),
                    );
                    jobs.push(TrackJob {
                        song: song.clone(),
                        variant,
                        multi,
                        out,
                    });
                }
            }
            Err(err) => {
                warnings.push(format!("{} \"{}\": {err}", song.song_id, song.title));
            }
        }
    }

    let mut seen = HashSet::new();
    jobs.retain(|job| {
        if seen.insert(job.out.clone()) {
            true
        } else {
            warnings.push(format!(
                "{} \"{}\": duplicate output {}, variant dropped",
                job.song.song_id,
                job.song.title,
                job.out.display()
            ));
            false
        }
    });

    Ok(Plan { jobs, warnings })
}

pub fn execute(
    args: &SdvxArgs,
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
        |job, collector, sink| process_job(job, &ffmpeg, args, collector, sink),
    )
}

fn display_title(job: &TrackJob) -> String {
    match (job.multi, &job.variant.diff_label) {
        (true, Some(label)) => format!("{} ({label})", job.song.title),
        _ => job.song.title.clone(),
    }
}

fn album_for(song: &Song, target_version: Option<&str>) -> String {
    let version = target_version.unwrap_or(&song.version);
    format!("SOUND VOLTEX {} {} GST", version, version_name(version))
}

fn output_path(
    out_root: &Path,
    target_version: Option<&str>,
    song: &Song,
    variant: &AudioVariant,
    multi: bool,
    ext: &str,
) -> PathBuf {
    let album = album_for(song, target_version);

    let artist = sanitize::clean(&song.artist);
    let title = sanitize::clean(&song.title);
    let mut stem = format!("{artist} - {title}");
    if let (true, Some(label)) = (multi, &variant.diff_label) {
        stem.push_str(&format!(" ({label})"));
    }
    sanitize::output_path(out_root, &album, &stem, ext)
}

fn process_job(
    job: &TrackJob,
    ffmpeg: &Path,
    args: &SdvxArgs,
    collector: &Collector,
    sink: &dyn ProgressSink,
) {
    let common = &args.common;
    let out = &job.out;

    sink.started(&job.song.title);

    if out.exists() && !common.overwrite {
        collector.skip(format!("skip existing {}", out.display()), sink);
        return;
    }

    if let Some(parent) = out.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            collector.fail(format!("cannot create {}: {err}", parent.display()), sink);
            return;
        }
    }

    let tags = build_tags(job, args);

    let cover = job.variant.jacket.as_deref();
    if cover.is_none() {
        collector.warn(
            format!("{} \"{}\": no jacket", job.song.song_id, job.song.title),
            sink,
        );
    }

    let encode_job = EncodeJob {
        input: EncodeInput::File(&job.variant.audio),
        output: out,
        format: common.format,
        tags: &tags,
        cover,
    };
    match ffmpeg::encode(ffmpeg, &encode_job) {
        Ok(()) => collector.ok(),
        Err(err) => {
            collector.fail(
                format!("{} \"{}\": {err}", job.song.song_id, job.song.title),
                sink,
            );
        }
    }
}

fn build_tags(job: &TrackJob, args: &SdvxArgs) -> TrackTags {
    let song = &job.song;
    let album = album_for(song, args.version.as_deref());

    let mut tags = TrackTags {
        title: display_title(job),
        artist: song.artist.clone(),
        album,
        album_artist: "Various Artists".to_string(),
        genre: song.genres.join(", "),
        track: song.song_id.clone(),
        bpm: Some(song.bpm),
        date: (song.release_date / 10000).to_string(),
        ..Default::default()
    };

    if args.version.is_none() {
        tags.disc = song.version.clone();
        tags.total_discs = TOTAL_VERSIONS.to_string();
    }
    tags
}
