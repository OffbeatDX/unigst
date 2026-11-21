# unigst

Unified game-soundtrack (GST) generator for arcade rhythm games (**SOUND VOLTEX**, **beatmania IIDX**, **pop'n music**).

## Requirements

**ffmpeg** on `PATH` (or next to the executable) — handles audio decoding and encoding.

## `unigst` vs `unigstgui`

Prebuilt Windows binaries are available on the [Releases](https://github.com/OffbeatDX/unigst/releases) page.

| Executable | Use |
|---|---|
| `unigst` | Command line only — smallest binary, no GUI dependencies. Best for scripts and batch runs. |
| `unigstgui` | Standalone desktop GUI (no console window). Auto-detects games, lets you filter/pick songs, and copies the equivalent `unigst` CLI command. |

## Build

```sh
cargo build --release                       # builds both unigst and unigstgui (build.bat copies them to repo root)
cargo build --release --no-default-features # builds unigst only (skips GUI dependencies)
```

## Usage

### GUI (`unigstgui`)

Run `unigstgui` (or double-click `unigstgui.exe`):

1. Pick the game folder — the game is auto-detected from its DLL + song data (`soundvoltex.dll` + `data/others/music_db.xml`, `bm2dx.dll` + `music_data.bin`, or `popn.dll`/`popn<N>.dll` + `data/sd`; DLL in `modules/` or the folder root).
2. Filter by version or search, sort by any column, and select songs (click toggles, Shift+click selects a range, header checkbox toggles all listed).
3. Click **Export** to encode.

Settings persist between launches, and **Copy command** copies the matching `unigst` CLI command.

### CLI (`unigst`)

```sh
unigst sdvx -i <SDVX contents dir>  -o <out dir> [options]
unigst iidx -i <IIDX contents dir>  -o <out dir> [options]
unigst popn -i <pop'n install dir>  -o <out dir> [options]
```

| Flag | Description | Default |
|---|---|---|
| `-i, --input` | Game contents root (folder containing `data/`) | — |
| `-o, --output` | Output root | `GST` |
| `-f, --format` | `opus` (192k VBR), `flac`, `mp3` (320k) | `opus` |
| `-j, --jobs` | Parallel workers | CPU count |
| `--overwrite` | Re-encode even if output exists | off |
| `--dry-run` | List planned outputs without encoding | off |
| `--json` | With `--dry-run`: output planned tracks as JSON | off |
| `--verbose` | Per-item logging instead of progress bar; with `--dry-run`, source/chart diagnostics | off |
| `-v, --version <N>` | Filter to one version (SDVX 1–7; IIDX style id; pop'n 1–29) | — |
| `--id <ID>...` | Filter to specific song IDs | — |
| `--dll <PATH>` | pop'n only: game DLL for music db (absolute or relative to `--input`) | `popn.dll` / `popn<N>.dll` in `modules/` or `--input` |

Outputs are written to `<output>/<Album>/<Artist> - <Title>.<ext>`.

## License

[MIT](LICENSE)
