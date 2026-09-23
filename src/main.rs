#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod engine;
mod i18n;
mod tools;
mod util;

use crate::config::Settings;
use crate::i18n::{apply_pref, t, tf, Key};

fn main() -> eframe::Result<()> {
    // GUI 없이 도구 상태만 확인/설치하는 보조 모드 (문제 해결·CI 용)
    let args: Vec<String> = std::env::args().skip(1).collect();
    // CLI 출력도 저장된 표시 언어를 따른다. (GUI 는 App::new 에서 다시 확정한다)
    apply_pref(Settings::load().language);
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
            println!("{}", tf(Key::CliHelp, &[env!("CARGO_PKG_VERSION")]));
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
            eprintln!("{}", tf(Key::CliIconLoadFailed, &[&e.to_string()]));
            None
        }
    }
}

fn cli_check() {
    let st = Settings::load();
    println!(
        "{}",
        tf(
            Key::CliInstallDir,
            &[&tools::managed_dir().display().to_string()]
        )
    );

    match tools::resolve(&st) {
        Some(p) => println!(
            "yt-dlp  : {}  ({})",
            tools::ytdlp_version(&p).unwrap_or_else(|| t(Key::CliRunFailed).into()),
            p.display()
        ),
        None => println!("yt-dlp  : {}", t(Key::CliNone)),
    }
    match tools::resolve_ffmpeg(&st) {
        Some(p) => println!(
            "ffmpeg  : {}  ({})",
            tools::ffmpeg_version(&p).unwrap_or_else(|| t(Key::CliRunFailed).into()),
            p.display()
        ),
        None => {
            println!("ffmpeg  : {}", t(Key::CliNone));
            if let Some(b) = tools::broken_ffmpeg(&st) {
                println!(
                    "          {}",
                    tf(Key::CliBrokenFound, &[&b.display().to_string()])
                );
            }
        }
    }
    println!(
        "{}",
        tf(
            Key::CliInstalledTags,
            &[
                &tools::installed_tag("yt-dlp").unwrap_or_else(|| t(Key::CliNoRecord).into()),
                &tools::installed_tag("ffmpeg").unwrap_or_else(|| t(Key::CliNoRecord).into()),
            ]
        )
    );
    let fail = |e: anyhow::Error| tf(Key::CliCheckFailed, &[&e.to_string()]);
    println!(
        "{}",
        tf(
            Key::CliLatestTags,
            &[
                &tools::ytdlp_latest().unwrap_or_else(fail),
                &tools::ffmpeg_latest().unwrap_or_else(fail),
            ]
        )
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
    println!("{}", tf(Key::CliInstalling, &["yt-dlp"]));
    match tools::install_ytdlp(&mut bar("yt-dlp")) {
        Ok((v, tag)) => println!("{}", tf(Key::CliInstalledOk, &["yt-dlp", &v, &tag])),
        Err(e) => eprintln!("{}", tf(Key::CliInstallFailed, &["yt-dlp", &e.to_string()])),
    }

    println!("{}", tf(Key::CliInstalling, &["ffmpeg"]));
    let mut ff = bar("ffmpeg");
    let mut fp = bar("ffprobe");
    match tools::install_ffmpeg(&mut |got, total, which| {
        if which == "ffmpeg" {
            ff(got, total)
        } else {
            fp(got, total)
        }
    }) {
        Ok((v, tag)) => println!("{}", tf(Key::CliInstalledOk, &["ffmpeg", &v, &tag])),
        Err(e) => eprintln!("{}", tf(Key::CliInstallFailed, &["ffmpeg", &e.to_string()])),
    }
}

/// GUI 없이 대기열 엔진을 그대로 돌린다. (동작 확인·문제 해결용)
fn cli_download(urls: &[String]) {
    use crate::engine::{Engine, Status};
    use std::sync::{Arc, Mutex};

    if urls.is_empty() {
        eprintln!("{}", t(Key::CliUsageDownload));
        return;
    }

    let settings = Settings::load();
    println!(
        "{}",
        tf(
            Key::CliSaveFolder,
            &[&settings.download_dir.display().to_string()]
        )
    );

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
    println!(
        "\n{}",
        tf(Key::CliSummary, &[&ok.to_string(), &bad.to_string()])
    );
    for j in jobs.iter() {
        if let Some(p) = &j.filepath {
            println!("  → {}", p.display());
        }
        if let Some(e) = &j.error {
            println!("  ! {} : {e}", j.title);
        }
    }
}
