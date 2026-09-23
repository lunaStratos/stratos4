//! 외부 도구(yt-dlp, ffmpeg/ffprobe)의 탐색·버전확인·설치·업데이트.
//!
//! 기본 정책은 "관리형(Managed)" — 앱 데이터 디렉터리 아래에 실행 파일을 직접 두고
//! 앱이 버전을 책임진다. yt-dlp 는 사이트 변경 대응 때문에 업데이트가 잦으므로
//! 시작할 때 최신 여부를 확인하고, 설정에 따라 자동으로 받아 적용한다.

use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::{app_dir, BinMode, Settings};
use crate::i18n::{t, tf, Key};
use crate::util::{new_command, which};

pub const YTDLP_REPO: &str = "yt-dlp/yt-dlp";
/// 플랫폼별 단일 실행 파일(gzip)을 제공해 압축 해제만으로 설치가 끝난다.
pub const FFMPEG_REPO: &str = "eugeneware/ffmpeg-static";
const UA: &str = concat!("stratos-dl/", env!("CARGO_PKG_VERSION"));

pub fn managed_dir() -> PathBuf {
    app_dir().join("bin")
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

// ─────────────────────────────────────────────────────────────
// 설치 기록
// ─────────────────────────────────────────────────────────────
//
// 실행 파일이 보고하는 버전과 릴리스 태그가 일치하지 않는 경우가 있다.
// (예: ffmpeg-static 의 b6.1.1 릴리스에 든 바이너리는 자신을 "6.0" 이라고 말한다)
// 그래서 최신 여부는 "앱이 어떤 태그를 설치했는지"를 따로 기록해서 판단한다.

fn record_path() -> PathBuf {
    managed_dir().join("installed.json")
}

fn read_record() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(record_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn installed_tag(tool: &str) -> Option<String> {
    read_record()
        .get(tool)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn set_installed_tag(tool: &str, tag: &str) {
    let mut m = read_record();
    m.insert(tool.to_string(), serde_json::Value::String(tag.to_string()));
    if let Some(d) = record_path().parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(
        record_path(),
        serde_json::to_string_pretty(&serde_json::Value::Object(m)).unwrap_or_default(),
    );
}

/// 릴리스 태그를 사람이 읽기 좋은 버전 문자열로 (`b6.1.1` → `6.1.1`)
pub fn pretty_tag(tag: &str) -> String {
    tag.trim_start_matches(['v', 'b']).to_string()
}

// ─────────────────────────────────────────────────────────────
// 다운로드 도우미
// ─────────────────────────────────────────────────────────────

/// 읽은 바이트 수를 세면서 진행률 콜백을 호출하는 리더
struct Counting<'a, R: Read> {
    inner: R,
    got: u64,
    total: u64,
    last: std::time::Instant,
    cb: &'a mut dyn FnMut(u64, u64),
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.got += n as u64;
        if n == 0 || self.last.elapsed().as_millis() > 100 {
            self.last = std::time::Instant::now();
            (self.cb)(self.got, self.total);
        }
        Ok(n)
    }
}

/// URL 을 받아 `dest` 에 쓴다. `gunzip` 이면 gzip 을 풀어서 저장한다.
fn download_to(
    url: &str,
    dest: &Path,
    gunzip: bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let mut resp = ureq::get(url)
        .header("User-Agent", UA)
        .call()
        .with_context(|| tf(Key::ErrDownloadFailed, &[url]))?;

    let total: u64 = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = dest.with_extension("part");
    {
        let mut file =
            std::io::BufWriter::new(std::fs::File::create(&tmp).context(t(Key::ErrTempFile))?);
        let counting = Counting {
            inner: resp.body_mut().as_reader(),
            got: 0,
            total,
            last: std::time::Instant::now(),
            cb: progress,
        };
        if gunzip {
            let mut gz = flate2::read::GzDecoder::new(counting);
            std::io::copy(&mut gz, &mut file).context(t(Key::ErrGunzip))?;
        } else {
            let mut r = counting;
            std::io::copy(&mut r, &mut file).context(t(Key::ErrFileWrite))?;
        }
    }

    // Windows 에서 기존 파일이 잠겨 있을 수 있으므로 한 번 치워 둔다.
    if dest.exists() {
        let old = dest.with_extension("old");
        let _ = std::fs::remove_file(&old);
        if std::fs::rename(dest, &old).is_err() {
            let _ = std::fs::remove_file(dest);
        }
    }
    std::fs::rename(&tmp, dest).context(t(Key::ErrReplaceBinary))?;
    make_executable(dest);
    Ok(())
}

fn make_executable(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(target_os = "macos")]
    {
        // Gatekeeper 격리 속성 제거 (없으면 "손상됨" 경고로 실행이 막힌다)
        let _ = new_command("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(p)
            .output();
    }
    let _ = p;
}

/// GitHub 최신 릴리스 태그
fn latest_tag(repo: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let body = ureq::get(&url)
        .header("User-Agent", UA)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context(t(Key::ErrGithubRelease))?
        .body_mut()
        .read_to_string()
        .context(t(Key::ErrReadBody))?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    v.get("tag_name")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!(t(Key::ErrNoTagName)))
}

// ─────────────────────────────────────────────────────────────
// yt-dlp
// ─────────────────────────────────────────────────────────────

pub fn ytdlp_asset() -> &'static str {
    if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else {
        "yt-dlp_linux"
    }
}

pub fn ytdlp_managed_path() -> PathBuf {
    managed_dir().join(exe("yt-dlp"))
}

/// 설정에 따라 실제로 실행할 yt-dlp 경로를 결정한다.
pub fn resolve(settings: &Settings) -> Option<PathBuf> {
    match settings.bin_mode {
        BinMode::Managed => {
            let p = ytdlp_managed_path();
            // 아직 내려받기 전이면 시스템 설치본으로 임시 대체
            if p.is_file() {
                Some(p)
            } else {
                which("yt-dlp")
            }
        }
        BinMode::System => which("yt-dlp"),
        BinMode::Custom => {
            let p = PathBuf::from(settings.ytdlp_custom_path.trim());
            p.is_file().then_some(p)
        }
    }
}

/// `<bin> --version` → "2026.08.19"
pub fn ytdlp_version(bin: &Path) -> Option<String> {
    let out = new_command(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn ytdlp_latest() -> Result<String> {
    latest_tag(YTDLP_REPO)
}

pub fn install_ytdlp(progress: &mut dyn FnMut(u64, u64)) -> Result<(String, String)> {
    // 무엇을 설치했는지 확실히 기록하기 위해 태그를 먼저 확인한다.
    let tag = latest_tag(YTDLP_REPO)?;
    let url = format!(
        "https://github.com/{YTDLP_REPO}/releases/download/{tag}/{}",
        ytdlp_asset()
    );
    let dest = ytdlp_managed_path();
    download_to(&url, &dest, false, progress)?;
    let ver = ytdlp_version(&dest).ok_or_else(|| anyhow!(t(Key::ErrInstalledButFailed)))?;
    set_installed_tag("yt-dlp", &tag);
    Ok((ver, tag))
}

/// 시스템/사용자 지정 설치본은 yt-dlp 자체 업데이트(-U)를 사용한다.
pub fn ytdlp_self_update(bin: &Path) -> Result<String> {
    let out = new_command(bin)
        .arg("-U")
        .output()
        .context(t(Key::ErrYtdlpUFailed))?;
    let mut msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !err.is_empty() {
        msg.push('\n');
        msg.push_str(&err);
    }
    if out.status.success() {
        Ok(msg)
    } else {
        Err(anyhow!(msg))
    }
}

// ─────────────────────────────────────────────────────────────
// ffmpeg / ffprobe
// ─────────────────────────────────────────────────────────────

/// (ffmpeg 자산명, ffprobe 자산명). 지원하지 않는 플랫폼이면 None.
pub fn ffmpeg_assets() -> Option<(String, String)> {
    let arch = std::env::consts::ARCH;
    let slug = if cfg!(target_os = "macos") {
        if arch == "aarch64" {
            "darwin-arm64"
        } else {
            "darwin-x64"
        }
    } else if cfg!(target_os = "windows") {
        // Windows ARM 에서도 x64 바이너리가 에뮬레이션으로 동작한다.
        "win32-x64"
    } else if cfg!(target_os = "linux") {
        match arch {
            "aarch64" => "linux-arm64",
            "arm" => "linux-arm",
            "x86" => "linux-ia32",
            _ => "linux-x64",
        }
    } else {
        return None;
    };
    Some((format!("ffmpeg-{slug}.gz"), format!("ffprobe-{slug}.gz")))
}

pub fn ffmpeg_managed_path() -> PathBuf {
    managed_dir().join(exe("ffmpeg"))
}

pub fn ffprobe_managed_path() -> PathBuf {
    managed_dir().join(exe("ffprobe"))
}

/// ffmpeg 후보 경로 (우선순위: 사용자 지정 → 관리형 → 시스템)
fn ffmpeg_candidates(settings: &Settings) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let custom = settings.ffmpeg_custom_path.trim();
    if !custom.is_empty() {
        let p = PathBuf::from(custom);
        v.push(if p.is_dir() { p.join(exe("ffmpeg")) } else { p });
    }
    v.push(ffmpeg_managed_path());
    if let Some(p) = which("ffmpeg") {
        v.push(p);
    }
    v.into_iter().filter(|p| p.is_file()).collect()
}

/// 파일 존재뿐 아니라 실제 실행 가능 여부까지 확인한다.
/// (Homebrew 업그레이드 중 라이브러리가 어긋나 실행만 실패하는 경우가 흔하다)
fn ffmpeg_works(path: &Path) -> bool {
    let cache = FFMPEG_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Some(v) = cache.lock().unwrap().get(path) {
        return *v;
    }
    let ok = ffmpeg_version(path).is_some();
    cache.lock().unwrap().insert(path.to_path_buf(), ok);
    ok
}

/// 설치 직후 재검사를 위해 실행 가능 여부 캐시를 비운다.
fn clear_ffmpeg_cache() {
    if let Some(c) = FFMPEG_CACHE.get() {
        c.lock().unwrap().clear();
    }
}

static FFMPEG_CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<PathBuf, bool>>> =
    std::sync::OnceLock::new();

/// `ffmpeg -version` 첫 줄에서 버전 문자열을 뽑는다. → "6.1.1"
pub fn ffmpeg_version(path: &Path) -> Option<String> {
    let out = new_command(path)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let first = s.lines().next()?;
    let mut it = first.split_whitespace();
    // "ffmpeg version 6.1.1-static ..."
    while let Some(t) = it.next() {
        if t == "version" {
            return it.next().map(|v| v.to_string());
        }
    }
    None
}

/// 실제로 동작하는 ffmpeg 경로
pub fn resolve_ffmpeg(settings: &Settings) -> Option<PathBuf> {
    ffmpeg_candidates(settings)
        .into_iter()
        .find(|p| ffmpeg_works(p))
}

/// 파일은 있으나 실행되지 않는 ffmpeg (안내 문구용)
pub fn broken_ffmpeg(settings: &Settings) -> Option<PathBuf> {
    let c = ffmpeg_candidates(settings);
    if c.iter().any(|p| ffmpeg_works(p)) {
        return None;
    }
    c.into_iter().next()
}

pub fn ffmpeg_latest() -> Result<String> {
    // 태그는 "b6.1.1" 형태
    latest_tag(FFMPEG_REPO)
}

/// 관리형 ffmpeg + ffprobe 를 내려받는다. (합계 약 40~55MB)
pub fn install_ffmpeg(progress: &mut dyn FnMut(u64, u64, &str)) -> Result<(String, String)> {
    let (ff, fp) = ffmpeg_assets().ok_or_else(|| anyhow!(t(Key::ErrNoFfmpegBuild)))?;
    let tag = latest_tag(FFMPEG_REPO)?;
    let base = format!("https://github.com/{FFMPEG_REPO}/releases/download/{tag}");

    let dest = ffmpeg_managed_path();
    download_to(&format!("{base}/{ff}"), &dest, true, &mut |g, t| {
        progress(g, t, "ffmpeg")
    })?;
    // ffprobe 는 일부 후처리에서만 쓰이므로 실패해도 치명적이지 않다.
    let _ = download_to(
        &format!("{base}/{fp}"),
        &ffprobe_managed_path(),
        true,
        &mut |g, t| progress(g, t, "ffprobe"),
    );

    clear_ffmpeg_cache();
    let ver = ffmpeg_version(&dest).ok_or_else(|| anyhow!(t(Key::ErrInstalledButFailed)))?;
    set_installed_tag("ffmpeg", &tag);
    Ok((ver, tag))
}

// ─────────────────────────────────────────────────────────────
// UI 가 공유하는 상태
// ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct ToolState {
    /// 실행 파일이 스스로 보고하는 버전 (표시용)
    pub local: Option<String>,
    /// 앱이 설치한 릴리스 태그 (최신 여부 판단의 기준)
    pub installed: Option<String>,
    /// 최신 릴리스 태그
    pub latest: Option<String>,
    /// 앱이 관리하는 복사본을 쓰고 있는지 (아니면 시스템 설치본)
    pub managed: bool,
    pub busy: bool,
    pub message: String,
    /// (받은 바이트, 전체 바이트, 라벨)
    pub progress: Option<(u64, u64, String)>,
    pub path: Option<PathBuf>,
}

impl ToolState {
    /// 관리형일 때만 최신 여부를 판단한다. 시스템 설치본은 사용자가 관리한다.
    ///
    /// 비교 기준은 실행 파일이 보고하는 버전이 아니라 설치 기록에 남은 릴리스 태그다.
    /// 둘이 어긋나는 배포본이 있어 그대로 비교하면 매번 재설치가 일어난다.
    pub fn update_available(&self) -> bool {
        if !self.managed {
            return false;
        }
        let Some(latest) = self.latest.as_deref() else {
            return false;
        };
        match self.installed.as_deref() {
            Some(cur) => cur.trim() != latest.trim(),
            // 설치 기록이 없으면(수동 배치·기록 유실) 한 번 갱신해 기준을 만든다.
            None => true,
        }
    }

    /// 표시용 최신 버전 문자열
    pub fn latest_pretty(&self) -> Option<String> {
        self.latest.as_deref().map(pretty_tag)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tools {
    pub ytdlp: ToolState,
    pub ffmpeg: ToolState,
}

pub type SharedTools = Arc<Mutex<Tools>>;

fn set_ytdlp<F: FnOnce(&mut ToolState)>(t: &SharedTools, ctx: &egui::Context, f: F) {
    f(&mut t.lock().unwrap().ytdlp);
    ctx.request_repaint();
}

fn set_ffmpeg<F: FnOnce(&mut ToolState)>(t: &SharedTools, ctx: &egui::Context, f: F) {
    f(&mut t.lock().unwrap().ffmpeg);
    ctx.request_repaint();
}

/// 현재 설치 상태만 빠르게 읽어 온다. (네트워크 접근 없음)
pub fn probe_local(tools: &SharedTools, settings: &Settings) {
    let yt = resolve(settings);
    let ff = resolve_ffmpeg(settings);
    let mut t = tools.lock().unwrap();
    t.ytdlp.managed = settings.bin_mode == BinMode::Managed;
    t.ytdlp.local = yt.as_deref().and_then(ytdlp_version);
    t.ytdlp.installed = installed_tag("yt-dlp");
    t.ytdlp.path = yt;
    t.ffmpeg.managed = ff
        .as_ref()
        .map(|p| *p == ffmpeg_managed_path())
        .unwrap_or(true);
    t.ffmpeg.local = ff.as_deref().and_then(ffmpeg_version);
    t.ffmpeg.installed = installed_tag("ffmpeg");
    t.ffmpeg.path = ff;
}

/// 시작 시 실행되는 점검 루틴.
/// 없으면 설치하고, 관리형이면 최신 버전과 비교해 (설정에 따라) 자동으로 갱신한다.
pub fn spawn_startup_check(
    tools: SharedTools,
    settings: Settings,
    ctx: egui::Context,
    force: bool,
) {
    std::thread::spawn(move || {
        // ── yt-dlp ──────────────────────────────────
        let managed = settings.bin_mode == BinMode::Managed;
        let bin = resolve(&settings);
        let local = bin.as_deref().and_then(ytdlp_version);
        set_ytdlp(&tools, &ctx, |s| {
            s.managed = managed;
            s.local = local.clone();
            s.installed = installed_tag("yt-dlp");
            s.path = bin.clone();
        });

        let need_install = managed && !ytdlp_managed_path().is_file();
        if need_install {
            do_install_ytdlp(&tools, &ctx);
        } else if settings.auto_update_check || force {
            set_ytdlp(&tools, &ctx, |s| {
                s.busy = true;
                s.message = t(Key::MsgCheckingLatest).into();
            });
            match ytdlp_latest() {
                Ok(v) => {
                    let outdated = {
                        let mut t = tools.lock().unwrap();
                        t.ytdlp.latest = Some(v.clone());
                        t.ytdlp.busy = false;
                        t.ytdlp.update_available()
                    };
                    ctx.request_repaint();
                    if outdated && managed && settings.auto_update_tools {
                        do_install_ytdlp(&tools, &ctx);
                    } else {
                        set_ytdlp(&tools, &ctx, |s| {
                            s.message = if outdated {
                                tf(Key::MsgNewVersion, &[&pretty_tag(&v)])
                            } else {
                                t(Key::MsgUpToDate).into()
                            };
                        });
                    }
                }
                Err(e) => set_ytdlp(&tools, &ctx, |s| {
                    s.busy = false;
                    s.message = tf(Key::MsgCheckFailed, &[&e.to_string()]);
                }),
            }
        }

        // ── ffmpeg ──────────────────────────────────
        let ff = resolve_ffmpeg(&settings);
        let ff_managed = ff
            .as_ref()
            .map(|p| *p == ffmpeg_managed_path())
            .unwrap_or(true);
        let ff_local = ff.as_deref().and_then(ffmpeg_version);
        set_ffmpeg(&tools, &ctx, |s| {
            s.managed = ff_managed;
            s.local = ff_local.clone();
            s.installed = installed_tag("ffmpeg");
            s.path = ff.clone();
        });

        if ff.is_none() {
            if settings.auto_install_ffmpeg {
                do_install_ffmpeg(&tools, &ctx);
            } else {
                set_ffmpeg(&tools, &ctx, |s| {
                    s.message = t(Key::MsgFfmpegLimited).into();
                });
            }
        } else if ff_managed && (settings.auto_update_check || force) {
            set_ffmpeg(&tools, &ctx, |s| {
                s.busy = true;
                s.message = t(Key::MsgCheckingLatest).into();
            });
            match ffmpeg_latest() {
                Ok(v) => {
                    let outdated = {
                        let mut t = tools.lock().unwrap();
                        t.ffmpeg.latest = Some(v.clone());
                        t.ffmpeg.busy = false;
                        t.ffmpeg.update_available()
                    };
                    ctx.request_repaint();
                    if outdated && settings.auto_update_tools {
                        do_install_ffmpeg(&tools, &ctx);
                    } else {
                        set_ffmpeg(&tools, &ctx, |s| {
                            s.message = if outdated {
                                tf(Key::MsgNewVersion, &[&pretty_tag(&v)])
                            } else {
                                t(Key::MsgUpToDate).into()
                            };
                        });
                    }
                }
                Err(e) => set_ffmpeg(&tools, &ctx, |s| {
                    s.busy = false;
                    s.message = tf(Key::MsgCheckFailed, &[&e.to_string()]);
                }),
            }
        } else {
            set_ffmpeg(&tools, &ctx, |s| s.message.clear());
        }
    });
}

fn do_install_ytdlp(tools: &SharedTools, ctx: &egui::Context) {
    set_ytdlp(tools, ctx, |s| {
        s.busy = true;
        s.message = t(Key::MsgDownloadingYtdlp).into();
        s.progress = Some((0, 0, "yt-dlp".into()));
    });
    let t = tools.clone();
    let c = ctx.clone();
    let result = install_ytdlp(&mut |got, total| {
        t.lock().unwrap().ytdlp.progress = Some((got, total, "yt-dlp".into()));
        c.request_repaint();
    });
    set_ytdlp(tools, ctx, |s| {
        s.busy = false;
        s.progress = None;
        match result {
            Ok((ver, tag)) => {
                s.message = tf(Key::MsgApplied, &["yt-dlp", &ver]);
                s.local = Some(ver);
                s.installed = Some(tag.clone());
                s.latest = Some(tag);
                s.path = Some(ytdlp_managed_path());
                s.managed = true;
            }
            Err(e) => s.message = tf(Key::MsgInstallFailed, &[&e.to_string()]),
        }
    });
}

fn do_install_ffmpeg(tools: &SharedTools, ctx: &egui::Context) {
    set_ffmpeg(tools, ctx, |s| {
        s.busy = true;
        s.message = t(Key::MsgDownloadingFfmpeg).into();
        s.progress = Some((0, 0, "ffmpeg".into()));
    });
    let t = tools.clone();
    let c = ctx.clone();
    let result = install_ffmpeg(&mut |got, total, label| {
        t.lock().unwrap().ffmpeg.progress = Some((got, total, label.to_string()));
        c.request_repaint();
    });
    set_ffmpeg(tools, ctx, |s| {
        s.busy = false;
        s.progress = None;
        match result {
            Ok((ver, tag)) => {
                s.message = tf(Key::MsgApplied, &["ffmpeg", &ver]);
                s.local = Some(ver);
                s.installed = Some(tag.clone());
                s.latest = Some(tag);
                s.path = Some(ffmpeg_managed_path());
                s.managed = true;
            }
            Err(e) => s.message = tf(Key::MsgInstallFailed, &[&e.to_string()]),
        }
    });
}

/// 버튼으로 수동 설치/업데이트를 실행한다.
pub fn spawn_install_ytdlp(tools: SharedTools, settings: Settings, ctx: egui::Context) {
    std::thread::spawn(move || {
        if settings.bin_mode == BinMode::Managed {
            do_install_ytdlp(&tools, &ctx);
        } else {
            set_ytdlp(&tools, &ctx, |s| {
                s.busy = true;
                s.message = t(Key::MsgRunningYtdlpU).into();
            });
            let r = match resolve(&settings) {
                Some(bin) => ytdlp_self_update(&bin).and_then(|_| {
                    ytdlp_version(&bin).ok_or_else(|| anyhow!(t(Key::ErrVersionCheckFailed)))
                }),
                None => Err(anyhow!(t(Key::ErrYtdlpBinNotFound))),
            };
            set_ytdlp(&tools, &ctx, |s| {
                s.busy = false;
                match r {
                    Ok(v) => {
                        s.message = tf(Key::MsgApplied, &["yt-dlp", &v]);
                        s.local = Some(v);
                    }
                    Err(e) => s.message = tf(Key::MsgUpdateFailed, &[&e.to_string()]),
                }
            });
        }
    });
}

pub fn spawn_install_ffmpeg(tools: SharedTools, ctx: egui::Context) {
    std::thread::spawn(move || do_install_ffmpeg(&tools, &ctx));
}
