#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Flac,
    Mp3,
    Opus,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Format::Flac => "flac",
            Format::Mp3 => "mp3",
            Format::Opus => "opus",
        }
    }

    pub fn muxer(self) -> &'static str {
        match self {
            Format::Flac => "flac",
            Format::Mp3 => "mp3",
            Format::Opus => "opus",
        }
    }

    pub fn codec_args(self) -> Vec<&'static str> {
        match self {
            Format::Flac => vec![
                "-c:a",
                "flac",
                "-compression_level",
                "8",
                "-sample_fmt",
                "s32",
                "-ac",
                "2",
            ],
            Format::Mp3 => vec![
                "-c:a",
                "libmp3lame",
                "-b:a",
                "320k",
                "-ac",
                "2",
                "-id3v2_version",
                "3",
            ],

            Format::Opus => vec![
                "-c:a",
                "libopus",
                "-b:a",
                "192k",
                "-vbr",
                "on",
                "-compression_level",
                "10",
                "-application",
                "audio",
                "-ac",
                "2",
            ],
        }
    }

    pub fn art_as_stream(self) -> bool {
        matches!(self, Format::Flac | Format::Mp3)
    }
}

impl std::str::FromStr for Format {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "flac" => Ok(Format::Flac),
            "mp3" => Ok(Format::Mp3),
            "opus" => Ok(Format::Opus),
            other => Err(format!("unknown format '{other}' (expected flac, mp3, or opus)")),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct TrackTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,

    pub track: String,

    pub disc: String,
    pub total_discs: String,

    pub date: String,
    pub bpm: Option<u32>,

    pub comment: Option<String>,
}

impl TrackTags {
    pub fn to_ffmpeg_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let mut push = |key: &str, value: &str| {
            if !value.is_empty() {
                args.push("-metadata".to_string());
                args.push(format!("{key}={value}"));
            }
        };
        push("title", &self.title);
        push("artist", &self.artist);
        push("album", &self.album);
        push("album_artist", &self.album_artist);
        push("genre", &self.genre);
        push("track", &self.track);
        push("disc", &self.disc);
        push("TOTALDISCS", &self.total_discs);
        push("date", &self.date);
        if let Some(bpm) = self.bpm {
            push("TBPM", &bpm.to_string());
            push("BPM", &bpm.to_string());
        }
        if let Some(comment) = &self.comment {
            push("comment", comment);
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_parses() {
        assert_eq!("flac".parse::<Format>().unwrap(), Format::Flac);
        assert_eq!("MP3".parse::<Format>().unwrap(), Format::Mp3);
        assert_eq!("opus".parse::<Format>().unwrap(), Format::Opus);
        assert!("wav".parse::<Format>().is_err());
    }

    #[test]
    fn only_flac_and_mp3_use_stream_art() {
        assert!(Format::Flac.art_as_stream());
        assert!(Format::Mp3.art_as_stream());

        assert!(!Format::Opus.art_as_stream());
    }

    #[test]
    fn tags_skip_empty_fields() {
        let tags = TrackTags {
            title: "Song".into(),
            track: "42".into(),
            ..Default::default()
        };
        let args = tags.to_ffmpeg_args();
        assert!(args.contains(&"title=Song".to_string()));
        assert!(args.contains(&"track=42".to_string()));

        assert!(!args.iter().any(|a| a.starts_with("artist=")));
        assert!(!args.iter().any(|a| a == "album="));
    }
}
