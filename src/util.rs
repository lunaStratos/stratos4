//! 공용 유틸리티: URL 추출, 포맷터, 폰트, 실행파일 탐색

use std::path::{Path, PathBuf};

/// 텍스트에서 http(s) URL 을 모두 추출한다. (클립보드 붙여넣기 처리용)
pub fn find_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &text[i..];
        let start = match rest.find("http") {
            Some(p) => i + p,
            None => break,
        };
        let tail = &text[start..];
        if !(tail.starts_with("http://") || tail.starts_with("https://")) {
            i = start + 4;
            continue;
        }
        let end_rel = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
            .unwrap_or(tail.len());
        let mut url = &tail[..end_rel];
        // 문장 끝 구두점 제거
        while let Some(last) = url.chars().last() {
            if matches!(last, '.' | ',' | ')' | ']' | '}' | ';' | '!' | '?') {
                url = &url[..url.len() - last.len_utf8()];
            } else {
                break;
            }
        }
        if url.len() > 10 {
            let u = url.to_string();
            if !out.contains(&u) {
                out.push(u);
            }
        }
        i = start + end_rel.max(1);
    }
    out
}

/// 플레이리스트/채널로 보이는 URL 인지 추정한다.
pub fn looks_like_playlist(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    let markers = [
        "list=",
        "/playlist",
        "/playlists/",
        "/sets/",
        "/channel/",
        "/@",
        "/c/",
        "/user/",
        "/album/",
        "/series/",
        "/collection/",
        "&start_radio=",
    ];
    if l.contains("/videos") || l.contains("/shorts?") || l.contains("/streams") {
        return true;
    }
    markers.iter().any(|m| l.contains(m))
}

pub fn fmt_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if b == 0 {
        return "-".into();
    }
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", b, U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn fmt_speed(bps: f64) -> String {
    if bps <= 0.0 {
        return "-".into();
    }
    format!("{}/s", fmt_bytes(bps as u64))
}

pub fn fmt_eta(sec: i64) -> String {
    if sec <= 0 {
        return "-".into();
    }
    let (h, m, s) = (sec / 3600, (sec % 3600) / 60, sec % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 따옴표를 인식하는 간단한 인자 분리기 (`추가 인자` 설정용)
pub fn shell_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    has = true;
                } else if c.is_whitespace() {
                    if has || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// PATH 및 플랫폼 공통 경로에서 실행파일을 찾는다.
/// macOS 의 .app 번들은 PATH 가 빈약하므로 Homebrew 경로를 직접 확인한다.
pub fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };

    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        for extra in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/opt/local/bin",
        ] {
            let p = PathBuf::from(extra);
            if !dirs.contains(&p) {
                dirs.push(p);
            }
        }
        if let Some(home) = directories::UserDirs::new() {
            dirs.push(home.home_dir().join(".local/bin"));
        }
    }

    for d in dirs {
        for e in &exts {
            let cand = d.join(format!("{name}{e}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 파일을 탐색기/파인더에서 선택된 상태로 연다.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
        return;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .creation_flags(0x0800_0000)
            .spawn();
        return;
    }
    #[allow(unreachable_code)]
    {
        if let Some(dir) = path.parent() {
            let _ = open::that_detached(dir);
        }
    }
}

/// 한글이 네모로 깨지지 않도록 시스템 한글 폰트를 egui 에 주입한다.
pub fn install_korean_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        // macOS
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/Library/Fonts/NanumGothic.ttf",
        // Windows
        "C:\\Windows\\Fonts\\malgun.ttf",
        "C:\\Windows\\Fonts\\MalgunGothic.ttf",
        "C:\\Windows\\Fonts\\gulim.ttc",
        // Linux
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];

    let Some((name, bytes)) = CANDIDATES.iter().find_map(|p| {
        std::fs::read(p).ok().map(|b| {
            (
                Path::new(p)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                b,
            )
        })
    }) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        name.clone(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push(name.clone());
    }
    ctx.set_fonts(fonts);
}

/// Windows 에서 자식 프로세스의 콘솔 창이 깜빡이지 않도록 플래그를 적용한 Command 생성기.
pub fn new_command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}
