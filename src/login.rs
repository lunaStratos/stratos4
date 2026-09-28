//! 앱 전용 브라우저 프로필로 로그인하기.
//!
//! 앱 안에 웹뷰를 넣으면 Google 이 로그인을 막으므로, 설치된 실제 브라우저를
//! 앱 전용 프로필로 띄워 사용자가 거기서 로그인하게 한다. 이후 yt-dlp 는
//! `--cookies-from-browser <브라우저>:<프로필 경로>` 로 그 쿠키만 읽는다.
//! 평소 쓰는 브라우저 프로필과 분리되어 있고 로그인 외에는 쓰지 않으므로
//! YouTube 쿠키가 잘 바뀌지 않아 로그인이 오래 유지된다.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{app_dir, Settings};
use crate::i18n::{t, tf, Key};
use crate::util::new_command;

/// 로그인 창에서 처음 여는 주소
const LOGIN_URL: &str =
    "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2F";
/// 로그인해야만 열리는 목록 ('나중에 볼 동영상') — 로그인 확인에 쓴다
const CHECK_URL: &str = "https://www.youtube.com/playlist?list=WL";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Browser {
    Firefox,
    Chrome,
    Edge,
    Brave,
    Chromium,
}

impl Browser {
    /// 추천 순서. Firefox 는 Windows 의 앱 바인딩 암호화나 macOS 키체인 문제가 없어 가장 확실하다.
    pub const ALL: [Browser; 5] = [
        Browser::Firefox,
        Browser::Chrome,
        Browser::Edge,
        Browser::Brave,
        Browser::Chromium,
    ];

    /// yt-dlp 의 브라우저 이름이자 설정에 저장하는 값
    pub fn id(self) -> &'static str {
        match self {
            Browser::Firefox => "firefox",
            Browser::Chrome => "chrome",
            Browser::Edge => "edge",
            Browser::Brave => "brave",
            Browser::Chromium => "chromium",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Browser::Firefox => "Firefox",
            Browser::Chrome => "Google Chrome",
            Browser::Edge => "Microsoft Edge",
            Browser::Brave => "Brave",
            Browser::Chromium => "Chromium",
        }
    }

    pub fn from_id(s: &str) -> Option<Browser> {
        Browser::ALL.into_iter().find(|b| b.id() == s.trim())
    }

    fn is_firefox(self) -> bool {
        self == Browser::Firefox
    }

    /// 설치된 실행 파일 위치
    pub fn find(self) -> Option<PathBuf> {
        candidates(self).into_iter().find(|p| p.is_file())
    }
}

#[cfg(target_os = "macos")]
fn candidates(b: Browser) -> Vec<PathBuf> {
    let (app, bin) = match b {
        Browser::Firefox => ("Firefox.app", "firefox"),
        Browser::Chrome => ("Google Chrome.app", "Google Chrome"),
        Browser::Edge => ("Microsoft Edge.app", "Microsoft Edge"),
        Browser::Brave => ("Brave Browser.app", "Brave Browser"),
        Browser::Chromium => ("Chromium.app", "Chromium"),
    };
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
        .into_iter()
        .map(|r| r.join(app).join("Contents/MacOS").join(bin))
        .collect()
}

#[cfg(windows)]
fn candidates(b: Browser) -> Vec<PathBuf> {
    let rel = match b {
        Browser::Firefox => r"Mozilla Firefox\firefox.exe",
        Browser::Chrome => r"Google\Chrome\Application\chrome.exe",
        Browser::Edge => r"Microsoft\Edge\Application\msedge.exe",
        Browser::Brave => r"BraveSoftware\Brave-Browser\Application\brave.exe",
        Browser::Chromium => r"Chromium\Application\chrome.exe",
    };
    ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|r| PathBuf::from(r).join(rel))
        .collect()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn candidates(b: Browser) -> Vec<PathBuf> {
    let names: &[&str] = match b {
        Browser::Firefox => &["firefox"],
        Browser::Chrome => &["google-chrome", "google-chrome-stable"],
        Browser::Edge => &["microsoft-edge", "microsoft-edge-stable"],
        Browser::Brave => &["brave-browser", "brave"],
        Browser::Chromium => &["chromium", "chromium-browser"],
    };
    names.iter().filter_map(|n| crate::util::which(n)).collect()
}

/// 이 PC 에 설치된 지원 브라우저 (추천 순)
pub fn installed() -> Vec<(Browser, PathBuf)> {
    Browser::ALL
        .into_iter()
        .filter_map(|b| b.find().map(|p| (b, p)))
        .collect()
}

/// 앱 전용 브라우저 프로필 (Chromium 계열은 user-data-dir)
pub fn profile_dir(b: Browser) -> PathBuf {
    app_dir().join("login").join(b.id())
}

/// yt-dlp 에 넘길 프로필 경로.
/// Chromium 계열은 `Default` 를 넘겨야 yt-dlp 가 그 위(user-data-dir)에서
/// `Local State`(Windows 복호화 키)를 다른 브라우저 것과 헷갈리지 않고 찾는다.
fn cookie_profile(b: Browser) -> PathBuf {
    let dir = profile_dir(b);
    if b.is_firefox() {
        dir
    } else {
        dir.join("Default")
    }
}

/// 전용 프로필에 쿠키 저장소가 만들어졌는지 (한 번이라도 로그인 창을 열고 닫았는지)
pub fn has_profile(b: Browser) -> bool {
    let p = cookie_profile(b);
    if b.is_firefox() {
        p.join("cookies.sqlite").is_file()
    } else {
        p.join("Network").join("Cookies").is_file() || p.join("Cookies").is_file()
    }
}

/// `--cookies-from-browser` 값
fn ytdlp_arg(b: Browser) -> String {
    format!("{}:{}", b.id(), cookie_profile(b).display())
}

/// 설정의 `login_browser` 로 앱 전용 로그인을 쓸 수 있으면 `--cookies-from-browser` 값을 준다.
pub fn active_arg(login_browser: &str) -> Option<String> {
    Browser::from_id(login_browser)
        .filter(|b| has_profile(*b))
        .map(ytdlp_arg)
}

/// 전용 프로필을 지운다 (= 로그아웃). 평소 쓰는 브라우저에는 영향이 없다.
pub fn logout(b: Browser) -> std::io::Result<()> {
    let dir = profile_dir(b);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
// 상태 / 백그라운드 작업
// ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// 로그인 창(브라우저)이 열려 있다
    BrowserOpen,
    Checking,
}

#[derive(Clone, Debug, Default)]
pub struct LoginState {
    pub phase: Phase,
    /// 마지막 확인 결과 (None = 확인 안 함)
    pub signed_in: Option<bool>,
    /// 오류 등 추가 안내
    pub message: String,
}

pub type SharedLogin = Arc<Mutex<LoginState>>;

/// 로그인 창을 띄우고, 사용자가 브라우저를 종료하면 자동으로 로그인 여부를 확인한다.
pub fn spawn_login(
    state: SharedLogin,
    b: Browser,
    exe: PathBuf,
    settings: Arc<Mutex<Settings>>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let dir = profile_dir(b);
        let spawned = std::fs::create_dir_all(&dir).and_then(|_| {
            let mut c = Command::new(&exe);
            if b.is_firefox() {
                c.arg("-profile")
                    .arg(&dir)
                    .args(["-no-remote", "-new-instance"]);
            } else {
                c.arg(format!("--user-data-dir={}", dir.display()))
                    .args(["--no-first-run", "--no-default-browser-check"]);
            }
            c.arg(LOGIN_URL)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        });
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let mut s = state.lock().unwrap();
                s.phase = Phase::Idle;
                s.message = tf(Key::LoginErrLaunch, &[b.name(), &e.to_string()]);
                ctx.request_repaint();
                return;
            }
        };
        {
            let mut s = state.lock().unwrap();
            s.phase = Phase::BrowserOpen;
            s.signed_in = None;
            s.message.clear();
        }
        ctx.request_repaint();

        let started = Instant::now();
        let _ = child.wait();

        // 같은 프로필 창이 이미 떠 있으면 새 프로세스는 거기에 넘기고 곧바로 끝난다.
        // 이때는 브라우저가 아직 열려 있으므로 확인하지 않고 사용자가 누르기를 기다린다.
        if started.elapsed() < Duration::from_secs(5) {
            ctx.request_repaint();
            return;
        }
        let st = settings.lock().unwrap().clone();
        run_check(&state, b, &st, &ctx);
    });
}

/// 로그인 여부만 다시 확인한다.
pub fn spawn_check(state: SharedLogin, b: Browser, st: Settings, ctx: egui::Context) {
    std::thread::spawn(move || run_check(&state, b, &st, &ctx));
}

fn run_check(state: &SharedLogin, b: Browser, st: &Settings, ctx: &egui::Context) {
    {
        let mut s = state.lock().unwrap();
        s.phase = Phase::Checking;
        s.message.clear();
    }
    ctx.request_repaint();

    let result = match crate::tools::resolve(st) {
        Some(bin) => check(&bin, b, st),
        None => Err(t(Key::ErrYtdlpNotFoundHint).to_string()),
    };

    let mut s = state.lock().unwrap();
    s.phase = Phase::Idle;
    match result {
        Ok(v) => s.signed_in = Some(v),
        Err(e) => {
            s.signed_in = None;
            s.message = e;
        }
    }
    drop(s);
    ctx.request_repaint();
}

/// 로그인해야만 열리는 목록을 읽어 본다. Ok(true) = 로그인됨, Ok(false) = 로그인 안 됨.
fn check(bin: &Path, b: Browser, st: &Settings) -> Result<bool, String> {
    if !has_profile(b) {
        return Ok(false);
    }
    let mut c = new_command(bin);
    if st.ignore_yt_dlp_config {
        c.arg("--ignore-config");
    }
    c.args([
        "--flat-playlist",
        "--playlist-items",
        "1",
        "--dump-single-json",
        "--no-colors",
        "--cookies-from-browser",
    ])
    .arg(ytdlp_arg(b));
    if !st.proxy.trim().is_empty() {
        c.arg("--proxy").arg(st.proxy.trim());
    }
    let out = c
        .arg(CHECK_URL)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| tf(Key::ErrSpawnFailed, &[&e.to_string()]))?;
    if out.status.success() {
        return Ok(true);
    }

    let err = String::from_utf8_lossy(&out.stderr);
    // Windows 의 Chrome/Edge 127+ 는 앱 바인딩 암호화(v20)로 쿠키를 외부에서 못 읽는다.
    if err.contains("v20") && !b.is_firefox() {
        return Err(t(Key::LoginErrAppBound).to_string());
    }
    if err.lines().any(crate::engine::needs_login) {
        return Ok(false);
    }
    Err(err
        .lines()
        .find(|l| l.starts_with("ERROR"))
        .unwrap_or_else(|| t(Key::ErrUnknownExit))
        .to_string())
}
