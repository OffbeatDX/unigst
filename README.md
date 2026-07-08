# unigst

Unified game-soundtrack (GST) generator for arcade rhythm games

## Requirements

**ffmpeg** on `PATH` (or next to the executable). Decodes/encodes all audio.

## Build

```
cargo build --release
```

## Usage

```
unigst sdvx -i <KFC contents dir>   -o <out dir> [options]
unigst iidx -i <IIDX contents dir>  -o <out dir> [options]
unigst popn -i <pop'n install dir>  -o <out dir> [options]
```

| Flag | Description | Default |
|------|-------------|---------|
| `-i, --input` | Game contents root (folder containing `data/`) | — |
| `-o, --output` | Output root | `GST` |
| `-f, --format` | `flac`, `mp3` (320k), `opus` (192k VBR) | `flac` |
| `-j, --jobs` | Parallel workers | CPU count |
| `--overwrite` | Re-encode even if output exists | off |
| `--dry-run` | List planned outputs, no audio work | off |
| `--json` | With `--dry-run`: planned tracks as JSON | off |
| `--verbose` | Per-item logging instead of progress bar | off |
| `-v, --version <N>` | Only this version (SDVX 1-7; IIDX style id; pop'n 1-29) | — |
| `--id <ID>...` | Only these song IDs | — |
| `--dll <PATH>` | pop'n only: popn.dll to read the music db from (absolute, or relative to `--input`) | `modules/popn.dll` |

Output named `<Artist> - <Title>.<ext>`.

## License

[MIT](LICENSE).
