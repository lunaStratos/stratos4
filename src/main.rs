#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod engine;
mod tools;
mod util;

use crate::config::Settings;

fn main() -> eframe::Result<()> {
    // GUI 없이 도구 상태만 확인/설치하는 보조 모드 (문제 해결·CI 용)
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("--check-tools") => {
            cli_check();
            return Ok(());
        }
        Some("--install-tools") => {
            cli_install();
            return Ok(());
        }
        Some("--download") => {
            cli_download(&args[1..]);
            return Ok(());
        }
        Some("-h" | "--help") => {
            println!(
                "stratos4 YT Downloader {}\n\n  (인자 없음)        GUI 실행\n  --check-tools    yt-dlp/ffmpeg 설치 상태와 최신 버전 확인\n  --install-tools  yt-dlp/ffmpeg 최신판을 앱 폴더에 설치\n  --download URL…  GUI 없이 저장된 설정 그대로 내려받기",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        _ => {}
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([940.0, 700.0])
        .with_min_inner_size([680.0, 420.0])
        .with_title("stratos4 YT Downloader");
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "stratos4 YT Downloader",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// 창·작업표시줄 아이콘. 실행 파일에 포함하므로 별도 파일이 필요 없다.
/// (macOS 의 Dock 아이콘은 .app 번들의 AppIcon.icns 가 담당한다.)
fn load_icon() -> Option<std::sync::Arc<egui::IconData>> {
    const ICON_PNG: &[u8] = include_bytes!("../img/icon.png");
    match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => Some(std::sync::Arc::new(icon)),
        Err(e) => {
            eprintln!("아이콘을 불러오지 못했습니다: {e}");
            None
        }
    }
}

fn cli_check() {
    let st = Settings::load();
    println!("설치 위치: {}", tools::managed_dir().display());

    match tools::resolve(&st) {
        Some(p) => println!(
            "yt-dlp  : {}  ({})",
            tools::ytdlp_version(&p).unwrap_or_else(|| "실행 실패".into()),
            p.display()
        ),
        None => println!("yt-dlp  : 없음"),
    }
    match tools::resolve_ffmpeg(&st) {
        Some(p) => println!(
            "ffmpeg  : {}  ({})",
            tools::ffmpeg_version(&p).unwrap_or_else(|| "실행 실패".into()),
            p.display()
        ),
        None => {
            println!("ffmpeg  : 없음");
            if let Some(b) = tools::broken_ffmpeg(&st) {
                println!("          (실행 불가 파일 발견: {})", b.display());
            }
        }
    }
    println!(
        "설치 태그: yt-dlp {} / ffmpeg {}",
        tools::installed_tag("yt-dlp").unwrap_or_else(|| "기록 없음".into()),
        tools::installed_tag("ffmpeg").unwrap_or_else(|| "기록 없음".into())
    );
    println!(
        "최신 태그: yt-dlp {} / ffmpeg {}",
        tools::ytdlp_latest().unwrap_or_else(|e| format!("확인 실패 ({e})")),
        tools::ffmpeg_latest().unwrap_or_else(|e| format!("확인 실패 ({e})"))
    );
}

/// 2MB 단위로만 진행률을 찍는 콘솔 표시기
fn bar(label: &'static str) -> impl FnMut(u64, u64) {
    let mut last = 0u64;
    move |got: u64, total: u64| {
        if got < last + 2 * 1024 * 1024 && got != total {
            return;
        }
        last = got;
        if total > 0 {
            println!(
                "{label}  {:>5.1}%  {got}/{total}",
                got as f64 / total as f64 * 100.0
            );
        } else {
            println!("{label}  {got} bytes");
        }
    }
}

fn cli_install() {
    println!("yt-dlp 설치 중...");
    match tools::install_ytdlp(&mut bar("yt-dlp")) {
        Ok((v, tag)) => println!("yt-dlp {v} 설치 완료 (릴리스 {tag})"),
        Err(e) => eprintln!("yt-dlp 설치 실패: {e}"),
    }

    println!("ffmpeg 설치 중...");
    let mut ff = bar("ffmpeg");
    let mut fp = bar("ffprobe");
    match tools::install_ffmpeg(&mut |got, total, which| {
        if which == "ffmpeg" {
            ff(got, total)
        } else {
            fp(got, total)
        }
    }) {
        Ok((v, tag)) => println!("ffmpeg {v} 설치 완료 (릴리스 {tag})"),
        Err(e) => eprintln!("ffmpeg 설치 실패: {e}"),
    }
}

/// GUI 없이 대기열 엔진을 그대로 돌린다. (동작 확인·문제 해결용)
fn cli_download(urls: &[String]) {
    use crate::engine::{Engine, Status};
    use std::sync::{Arc, Mutex};

    if urls.is_empty() {
        eprintln!("사용법: stratos-dl --download <URL> [URL...]");
        return;
    }

    let settings = Settings::load();
    println!("저장 폴더: {}", settings.download_dir.display());

    let ctx = egui::Context::default();
    let engine = Engine::new(ctx, Arc::new(Mutex::new(settings)));
    for u in urls {
        engine.add_url(u, None);
    }

    let mut shown: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let mut pending = false;
        {
            let jobs = engine.jobs.lock().unwrap();
            for j in jobs.iter() {
                if !j.status.is_finished() {
                    pending = true;
                }
                let line = format!(
                    "[{}] {} {:.1}% {}",
                    j.status.label(),
                    j.title,
                    j.progress * 100.0,
                    j.error.clone().unwrap_or_default()
                );
                if shown.get(&j.id) != Some(&line) {
                    // 진행률은 5% 단위로만 찍어 출력이 넘치지 않게 한다.
                    let coarse = format!(
                        "[{}] {} {}",
                        j.status.label(),
                        j.title,
                        (j.progress * 20.0) as u32
                    );
                    if shown.get(&j.id).map(|p| p.as_str()) != Some(coarse.as_str()) {
                        println!("{line}");
                    }
                    shown.insert(j.id, coarse);
                }
            }
        }
        if !pending {
            break;
        }
    }

    let jobs = engine.jobs.lock().unwrap();
    let ok = jobs.iter().filter(|j| j.status == Status::Done).count();
    let bad = jobs.iter().filter(|j| j.status == Status::Failed).count();
    println!("\n완료 {ok} / 실패 {bad}");
    for j in jobs.iter() {
        if let Some(p) = &j.filepath {
            println!("  → {}", p.display());
        }
        if let Some(e) = &j.error {
            println!("  ! {} : {e}", j.title);
        }
    }
}
