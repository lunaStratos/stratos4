# stratos4 YT Downloader

*[한국어](README.md) · English*

A cross-platform (macOS / Windows) downloader app built on yt-dlp. Written in Rust + egui,
it ships as a single executable with no external runtime.

## Screenshots

| Main window | Settings |
|-------------|----------|
| ![Main window](img/img-1.png) | ![Settings window](img/img-2.png) |

Pasting a URL drops it straight into the queue, and the top bar changes the download type,
quality, and playlist handling without opening the settings window.

## Features

- **Paste to download** — press `Ctrl/Cmd+V` in the window and the URL on the clipboard goes
  straight into the queue. If the clipboard holds several URLs, all of them are added.
  You can also drag and drop a text file containing URLs.
- **Playlists / channels** — paste a list URL as-is. Three handling modes:
  - `Expand into items` (default) — reads the list and creates one job per entry, so you get
    per-item progress, retries, and concurrent downloads.
  - `One batch job` — yt-dlp processes the whole list in order as a single job.
  - `This video only` — ignores the `list=` parameter and grabs just that video.
- **Quick settings** — change `Download (video/audio)`, `Quality`, and `Playlist handling`
  from the top bar without opening the settings window.
- **Automatic yt-dlp / ffmpeg management** — on startup the app checks whether they are
  installed and whether a newer release exists, downloads them if missing, and updates them
  when a new version appears. Nothing has to be installed system-wide.
- **Pause / resume** — pausing leaves the partial file behind, so restarting continues where
  it left off.
- Configurable concurrent downloads, concurrent fragments, rate limit, proxy, browser cookies,
  SponsorBlock, subtitles, thumbnail and metadata embedding, filename templates, and separate
  output folders for video and audio.

## Version history

- **1.0** — downloading confirmed working

## Building

Requires Rust 1.85 or newer. Everything is automated through `make`.

```bash
make                  # dev build, then run
make build            # release build → target/release/stratos-dl
make mac              # macOS .app + zip (current architecture)
make mac-universal    # universal Intel + Apple Silicon .app + zip
make win              # Windows .exe + zip (cross toolchain required)
make dist             # package every platform that is possible here
make check            # fmt + clippy(-D warnings) + release build
make icons            # regenerate .icns / .ico from img/icon.png
make status / tools   # check yt-dlp/ffmpeg status / install the latest
make clean            # wipe target/ and dist/
```

Artifacts land in `dist/` as `stratos-dl-<version>-<platform>.zip`.
The macOS app is ad-hoc signed automatically. With a real certificate, pass it explicitly:
`CODESIGN_ID="Developer ID Application: ..." make mac`.

### Icons

A single `img/icon.png` is the source. After changing it, regenerate the derived icons.

```bash
make icons        # → img/AppIcon.icns (macOS), img/icon.ico (Windows)
```

Commit the generated files — CI runners use what is committed rather than running the script.

| Where | Source |
|-------|--------|
| Window / taskbar | `img/icon.png` embedded in the binary (`include_bytes!`) |
| macOS Dock & Finder | `AppIcon.icns` inside the `.app` bundle |
| Windows Explorer | `build.rs` embeds `icon.ico` as an `.exe` resource |

If the source is smaller than 1024px the larger sizes are upscaled and look soft;
`make icons` warns about it. Embedding the Windows resource needs `rc.exe` (MSVC) or
`windres` (mingw) — without one, the build continues and only the icon is skipped.

### Building the Windows executable on macOS

Install the cross toolchain once and `make win` works.

```bash
./scripts/setup-cross.sh          # cargo-xwin (MSVC ABI, identical to the CI artifact)
./scripts/setup-cross.sh mingw    # mingw-w64 (GNU ABI)
```

If no toolchain is present, `make win` prints the installation instructions and exits.
Windows release builds run without a console window (`windows_subsystem = "windows"`).

### CI

`.github/workflows/build.yml` runs in three stages.

1. **check** — `cargo fmt --check` and `clippy -D warnings`
2. **build** — builds macOS arm64 / macOS x64 / Windows x64 in parallel and uploads artifacts
3. **release** — pushing a `v*` tag creates a GitHub Release with the three zips attached

```bash
git tag v0.1.0 && git push origin v0.1.0   # → release created automatically
```

## External tools

| Tool | Role | Default behavior |
|------|------|------------------|
| yt-dlp | Does the actual downloading | The app fetches and manages the standalone binary from the [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases) |
| ffmpeg / ffprobe | Muxing video+audio, audio conversion, thumbnail and metadata embedding | Installs a [static build](https://github.com/eugeneware/ffmpeg-static) automatically when missing |

Both tools are installed under the paths below, and the release tag that was installed is
recorded in `installed.json` to decide whether an update is needed. (Some distributions report
a binary version that disagrees with the release tag, so the tag is the source of truth.)

- macOS: `~/Library/Application Support/dev.stratos.stratos-dl/bin`
- Windows: `%APPDATA%\stratos\stratos-dl\data\bin`

To use a yt-dlp that is already installed system-wide, pick `System installation` under
Settings → Tools. For ffmpeg the check is not file existence but whether `-version` actually
runs, which also weeds out installations that are present but broken.

## Configuration file

- macOS: `~/Library/Application Support/dev.stratos.stratos-dl/settings.json`
- Windows: `%APPDATA%\stratos\stratos-dl\data\settings.json`

## Auxiliary CLI

Status checks and installation work without the GUI — handy for troubleshooting or CI.

```bash
stratos-dl --check-tools            # yt-dlp / ffmpeg install status and latest tags
stratos-dl --install-tools          # install the latest versions into the app folder
stratos-dl --download <URL>...      # headless download using the saved settings
```

## Layout

| File | Role |
|------|------|
| `src/main.rs` | Entry point, auxiliary CLI |
| `src/app.rs` | egui UI (top bar, queue, settings window) |
| `src/engine.rs` | Queue scheduler, yt-dlp process execution, progress parsing, playlist resolution |
| `src/config.rs` | Settings persistence, yt-dlp argument construction |
| `src/tools.rs` | yt-dlp / ffmpeg discovery, installation, updates |
| `src/util.rs` | URL extraction, formatters, Korean font injection, executable lookup |
| `Makefile` / `scripts/build.sh` | Build and packaging automation |
| `scripts/setup-cross.sh` | macOS → Windows cross-build toolchain installer |
| `build.rs` | Embeds the icon and version info into the Windows `.exe` |
| `scripts/make-icons.sh` | Generates `.icns` / `.ico` from `icon.png` |
| `img/` | Icon source and generated files |
| `.github/workflows/build.yml` | CI: lint → three-platform build → release on tag |
| `.vscode/` | VS Code tasks (⇧⌘B), debug configs (F5), recommended extensions |
