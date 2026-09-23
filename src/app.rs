//! egui 기반 UI

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::config::{BinMode, Mode, PlaylistMode, Settings, ThemePref};
use crate::engine::{Engine, Job, JobKind, Status};
use crate::i18n::{self, t, tf, Key, Lang};
use crate::tools::{self, SharedTools, ToolState, Tools};
use crate::util::{fmt_bytes, fmt_eta, fmt_speed};

enum Act {
    Start(u64),
    Pause(u64),
    Cancel(u64),
    Remove(u64),
    OpenFile(std::path::PathBuf),
    Reveal(std::path::PathBuf),
    CopyUrl(String),
}

pub struct App {
    engine: Engine,
    settings: Arc<Mutex<Settings>>,
    input: String,
    /// 추가 시 플레이리스트 처리 방식 (None = 설정값 사용)
    add_mode: Option<PlaylistMode>,
    show_settings: bool,
    /// 붙여넣기로 추가한 뒤 입력창을 비우기 위한 플래그
    clear_input_after_frame: bool,
    toast: Option<(String, Instant)>,

    clipboard: Option<arboard::Clipboard>,
    last_clip: String,
    last_clip_poll: Instant,

    tools: SharedTools,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_pixels_per_point(1.15);

        let mut settings = Settings::load();
        // 표시 언어를 먼저 확정해야 그 문자에 맞는 폰트를 올릴 수 있다.
        crate::util::install_fonts(&cc.egui_ctx, i18n::apply_pref(settings.language));
        apply_theme(&cc.egui_ctx, settings.theme);

        let settings_arc = Arc::new(Mutex::new(settings.clone()));
        let engine = Engine::new(cc.egui_ctx.clone(), settings_arc.clone());

        let mut clipboard = arboard::Clipboard::new().ok();
        // 앱 시작 시점에 이미 들어 있던 클립보드 내용은 자동 추가하지 않는다.
        let last_clip = clipboard
            .as_mut()
            .and_then(|c| c.get_text().ok())
            .unwrap_or_default();

        let tools: SharedTools = Arc::new(Mutex::new(Tools::default()));
        // 네트워크 없이 현재 설치 상태부터 즉시 표시한다.
        tools::probe_local(&tools, &settings);

        // 시작 시 점검: 없으면 설치하고, 관리형이면 최신 버전과 비교해 갱신한다.
        let now = chrono::Local::now().timestamp();
        let due = now - settings.last_update_check > 86_400;
        let missing = {
            let t = tools.lock().unwrap();
            t.ytdlp.local.is_none() || t.ffmpeg.local.is_none()
        };
        if missing || (settings.auto_update_check && due) {
            settings.last_update_check = now;
            settings.save();
            settings_arc.lock().unwrap().last_update_check = now;
            tools::spawn_startup_check(tools.clone(), settings.clone(), cc.egui_ctx.clone(), false);
        }

        Self {
            engine,
            settings: settings_arc,
            input: String::new(),
            add_mode: None,
            show_settings: false,
            clear_input_after_frame: false,
            toast: None,
            clipboard,
            last_clip,
            last_clip_poll: Instant::now(),
            tools,
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn settings_snapshot(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    fn save_settings(&self) {
        self.settings.lock().unwrap().save();
    }

    fn add_from_text(&mut self, text: &str) {
        self.add_from_text_inner(text, false);
    }

    /// `quiet` 면 URL 이 없을 때 아무 말도 하지 않는다.
    /// (전역 붙여넣기는 URL 이 아닌 텍스트에도 걸리므로 경고를 띄우지 않는다)
    fn add_from_text_inner(&mut self, text: &str, quiet: bool) {
        let urls = crate::util::find_urls(text);
        if urls.is_empty() {
            if !quiet {
                self.toast(t(Key::ToastNoUrl));
            }
            return;
        }
        let n = self.engine.add_many(&urls, self.add_mode);
        if n == 0 {
            self.toast(t(Key::ToastDuplicate));
        } else {
            self.toast(tf(Key::ToastAdded, &[&n.to_string()]));
        }
    }

    /// 클립보드를 주기적으로 확인해 새 URL 이 복사되면 자동으로 대기열에 넣는다.
    fn poll_clipboard(&mut self) {
        if !self.settings.lock().unwrap().clipboard_watch {
            return;
        }
        if self.last_clip_poll.elapsed() < Duration::from_millis(600) {
            return;
        }
        self.last_clip_poll = Instant::now();

        let Some(cb) = self.clipboard.as_mut() else {
            return;
        };
        let Ok(text) = cb.get_text() else { return };
        if text == self.last_clip || text.trim().is_empty() {
            return;
        }
        self.last_clip = text.clone();

        let urls = crate::util::find_urls(&text);
        if urls.is_empty() {
            return;
        }
        let n = self.engine.add_many(&urls, self.add_mode);
        if n > 0 {
            self.toast(tf(Key::ToastAddedClipboard, &[&n.to_string()]));
        }
    }

    fn refresh_tools(&mut self) {
        let st = self.settings_snapshot();
        tools::probe_local(&self.tools, &st);
    }
}

fn apply_theme(ctx: &egui::Context, pref: ThemePref) {
    match pref {
        ThemePref::System => ctx.set_theme(egui::ThemePreference::System),
        ThemePref::Light => ctx.set_theme(egui::ThemePreference::Light),
        ThemePref::Dark => ctx.set_theme(egui::ThemePreference::Dark),
    }
}

/// 상단 바의 도구 상태 칩 (버전 / 진행 / 경고)
fn tool_chip(ui: &mut egui::Ui, name: &str, st: &ToolState) -> egui::Response {
    let (text, color, tip) = if st.busy {
        let pct = st
            .progress
            .as_ref()
            .filter(|(_, t, _)| *t > 0)
            .map(|(g, t, _)| format!(" {:.0}%", *g as f32 / *t as f32 * 100.0))
            .unwrap_or_default();
        (
            format!("{}{pct}", tf(Key::ToolUpdating, &[name])),
            Color32::from_rgb(210, 175, 70),
            st.message.clone(),
        )
    } else {
        match &st.local {
            Some(v) if st.update_available() => (
                format!("{name} {v} ↑"),
                Color32::from_rgb(220, 150, 60),
                tf(
                    Key::ToolNewVersionAvailable,
                    &[&st.latest_pretty().unwrap_or_default()],
                ),
            ),
            Some(v) => (
                format!("{name} {v}"),
                Color32::from_rgb(120, 180, 130),
                st.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| st.message.clone()),
            ),
            None => (
                tf(Key::ToolMissing, &[name]),
                Color32::from_rgb(220, 90, 90),
                if st.message.is_empty() {
                    tf(Key::ToolNotInstalledHint, &[name])
                } else {
                    st.message.clone()
                },
            ),
        }
    };

    let resp = ui.add(
        egui::Label::new(RichText::new(text).small().color(color)).sense(egui::Sense::click()),
    );
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

fn status_color(s: Status) -> Color32 {
    match s {
        Status::Queued => Color32::from_rgb(140, 140, 150),
        Status::Running => Color32::from_rgb(60, 150, 230),
        Status::Paused => Color32::from_rgb(220, 170, 50),
        Status::Done => Color32::from_rgb(70, 180, 110),
        Status::Failed => Color32::from_rgb(220, 90, 90),
        Status::Canceled => Color32::from_rgb(140, 140, 150),
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_clipboard();

        // 창에서 붙여넣기(Ctrl/Cmd+V) 하거나 URL 이 담긴 텍스트 파일을 끌어다 놓은 경우
        let paste_to_download = self.settings.lock().unwrap().paste_to_download;
        let mut pasted: Vec<String> = Vec::new();
        let mut dropped: Vec<String> = Vec::new();
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Paste(text) = e {
                    pasted.push(text.clone());
                }
            }
            for f in &i.raw.dropped_files {
                if let Some(p) = &f.path {
                    if let Ok(s) = std::fs::read_to_string(p) {
                        dropped.push(s);
                    }
                } else if let Some(b) = &f.bytes {
                    dropped.push(String::from_utf8_lossy(b).into_owned());
                }
            }
        });

        // 설정 창의 입력란(프록시, 추가 인자 등)에 붙여넣는 것까지 가로채면 안 되므로
        // 포커스가 없거나 URL 입력창에 있을 때만 "붙여넣기 = 추가" 로 처리한다.
        let focus_ok = !self.show_settings
            && ctx.memory(|m| match m.focused() {
                None => true,
                Some(id) => id == url_input_id(),
            });
        if paste_to_download && focus_ok && !pasted.is_empty() {
            for text in &pasted {
                self.add_from_text_inner(text, true);
            }
            // 입력창이 포커스를 갖고 있었다면 이번 프레임에 같은 텍스트가 들어가므로
            // 프레임 끝에서 비워 준다.
            self.clear_input_after_frame = true;
        }
        for text in dropped {
            if !crate::util::find_urls(&text).is_empty() {
                self.add_from_text(&text);
            }
        }

        self.top_bar(ctx);
        self.bottom_bar(ctx);
        self.job_list(ctx);

        if self.show_settings {
            self.settings_window(ctx);
        }

        if self.clear_input_after_frame {
            self.clear_input_after_frame = false;
            self.input.clear();
        }

        // 진행 중 작업이 있으면 부드러운 갱신 유지
        let (running, queued, _, _) = self.engine.counts();
        if running > 0 || queued > 0 {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else {
            ctx.request_repaint_after(Duration::from_millis(700));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}

impl App {
    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("stratos4 YT Downloader");
                ui.label(RichText::new(t(Key::AppSubtitle)).weak().small());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("⚙  {}", t(Key::BtnSettings))).clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    let t = self.tools.lock().unwrap().clone();
                    if tool_chip(ui, "ffmpeg", &t.ffmpeg).clicked() {
                        self.show_settings = true;
                    }
                    if tool_chip(ui, "yt-dlp", &t.ytdlp).clicked() {
                        self.show_settings = true;
                    }
                });
            });

            // 도구를 내려받는 동안에만 보이는 진행 막대
            let dl = {
                let t = self.tools.lock().unwrap();
                t.ytdlp
                    .progress
                    .clone()
                    .or_else(|| t.ffmpeg.progress.clone())
            };
            if let Some((got, total, label)) = dl {
                let frac = if total > 0 {
                    got as f32 / total as f32
                } else {
                    0.0
                };
                let text = if total > 0 {
                    tf(
                        Key::ToolDownloadingSized,
                        &[&label, &fmt_bytes(got), &fmt_bytes(total)],
                    )
                } else {
                    tf(Key::ToolDownloading, &[&label, &fmt_bytes(got)])
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(10.0)
                        .text(RichText::new(text).small()),
                );
            }

            ui.add_space(4.0);

            // ── URL 입력 줄 ─────────────────────────────
            ui.horizontal(|ui| {
                let reserved = 150.0;
                let resp = ui.add_sized(
                    [(ui.available_width() - reserved).max(160.0), 28.0],
                    egui::TextEdit::singleline(&mut self.input)
                        .id(url_input_id())
                        .hint_text(t(Key::UrlHint)),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                if ui
                    .add_sized([56.0, 28.0], egui::Button::new(t(Key::BtnAdd)))
                    .clicked()
                    || enter
                {
                    let text = std::mem::take(&mut self.input);
                    if !text.trim().is_empty() {
                        self.add_from_text(&text);
                    }
                    resp.request_focus();
                }
                if ui
                    .add_sized(
                        [96.0, 28.0],
                        egui::Button::new(format!("📋 {}", t(Key::BtnPaste))),
                    )
                    .on_hover_text(t(Key::BtnPasteTip))
                    .clicked()
                {
                    let text = self
                        .clipboard
                        .as_mut()
                        .and_then(|c| c.get_text().ok())
                        .unwrap_or_default();
                    self.last_clip = text.clone();
                    self.add_from_text(&text);
                }
            });

            ui.add_space(4.0);

            // ── 빠른 설정 (설정 창을 열지 않고 바꾸는 항목) ──
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let mut st = self.settings.lock().unwrap();
                let mut changed = false;

                ui.label(RichText::new(t(Key::LblDownload)).weak());
                egui::ComboBox::from_id_salt("quick_mode")
                    .width(88.0)
                    .selected_text(mode_label(st.mode))
                    .show_ui(ui, |ui| {
                        for m in [Mode::Video, Mode::Audio] {
                            changed |= ui
                                .selectable_value(&mut st.mode, m, mode_label(m))
                                .changed();
                        }
                    });

                ui.add_space(8.0);
                ui.label(RichText::new(t(Key::LblQuality)).weak());
                if st.mode == Mode::Video {
                    egui::ComboBox::from_id_salt("quick_q")
                        .width(104.0)
                        .selected_text(height_label(st.max_height))
                        .show_ui(ui, |ui| {
                            for h in [0u32, 2160, 1440, 1080, 720, 480, 360] {
                                changed |= ui
                                    .selectable_value(&mut st.max_height, h, height_label(h))
                                    .changed();
                            }
                        });
                } else {
                    egui::ComboBox::from_id_salt("quick_ab")
                        .width(104.0)
                        .selected_text(bitrate_label(&st.audio_bitrate))
                        .show_ui(ui, |ui| {
                            for b in AUDIO_BITRATES {
                                let mut cur = st.audio_bitrate.clone();
                                if ui
                                    .selectable_value(&mut cur, b.to_string(), bitrate_label(b))
                                    .changed()
                                {
                                    st.audio_bitrate = cur;
                                    changed = true;
                                }
                            }
                        });

                    ui.add_space(8.0);
                    ui.label(RichText::new(t(Key::LblFormat)).weak());
                    egui::ComboBox::from_id_salt("quick_af")
                        .width(80.0)
                        .selected_text(st.audio_format.clone())
                        .show_ui(ui, |ui| {
                            for f in AUDIO_FORMATS {
                                let mut cur = st.audio_format.clone();
                                if ui.selectable_value(&mut cur, f.to_string(), f).changed() {
                                    st.audio_format = cur;
                                    changed = true;
                                }
                            }
                        });
                }

                ui.add_space(8.0);
                ui.label(RichText::new(t(Key::LblPlaylist)).weak());
                let cur = self.add_mode.unwrap_or(st.playlist_mode);
                let mut sel = cur;
                egui::ComboBox::from_id_salt("quick_pl")
                    .width(126.0)
                    .selected_text(playlist_mode_label(cur))
                    .show_ui(ui, |ui| {
                        for m in [
                            PlaylistMode::Expand,
                            PlaylistMode::Single,
                            PlaylistMode::VideoOnly,
                        ] {
                            ui.selectable_value(&mut sel, m, playlist_mode_label(m))
                                .on_hover_text(playlist_mode_hint(m));
                        }
                    });
                if sel != cur {
                    self.add_mode = Some(sel);
                }

                ui.add_space(8.0);
                changed |= ui
                    .checkbox(&mut st.clipboard_watch, t(Key::ChkClipboardQuick))
                    .on_hover_text(t(Key::ChkClipboardQuickTip))
                    .changed();

                if changed {
                    st.save();
                }
            });

            ui.add_space(6.0);
        });
    }

    fn bottom_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let (r, q, d, f) = self.engine.counts();
                ui.label(
                    RichText::new(tf(
                        Key::StatusCounts,
                        &[
                            &r.to_string(),
                            &q.to_string(),
                            &d.to_string(),
                            &f.to_string(),
                        ],
                    ))
                    .small(),
                );

                ui.separator();
                let dir = self.settings.lock().unwrap().target_dir();
                if ui
                    .link(RichText::new(format!("📁 {}", dir.display())).small())
                    .on_hover_text(t(Key::TipOpenFolder))
                    .clicked()
                {
                    let _ = open::that_detached(&dir);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t(Key::BtnClearFinished)).clicked() {
                        self.engine.clear_finished();
                    }
                    if ui.button(format!("⏸ {}", t(Key::BtnPauseAll))).clicked() {
                        self.engine.pause_all();
                    }
                    if ui.button(format!("▶ {}", t(Key::BtnStartAll))).clicked() {
                        self.engine.start_all();
                    }
                    if let Some((msg, at)) = &self.toast {
                        if at.elapsed() < Duration::from_secs(4) {
                            ui.label(
                                RichText::new(msg)
                                    .small()
                                    .color(Color32::from_rgb(110, 170, 220)),
                            );
                        } else {
                            self.toast = None;
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn job_list(&mut self, ctx: &egui::Context) {
        let mut acts: Vec<Act> = Vec::new();

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut jobs = self.engine.jobs.lock().unwrap();
            if jobs.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(t(Key::EmptyHint)).weak());
                });
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for j in jobs.iter_mut() {
                        job_row(ui, j, &mut acts);
                        ui.add_space(4.0);
                    }
                });
        });

        for a in acts {
            match a {
                Act::Start(id) => self.engine.start(id),
                Act::Pause(id) => self.engine.pause(id),
                Act::Cancel(id) => self.engine.cancel(id),
                Act::Remove(id) => self.engine.remove(id),
                Act::OpenFile(p) => {
                    let _ = open::that_detached(&p);
                }
                Act::Reveal(p) => crate::util::reveal(&p),
                Act::CopyUrl(u) => {
                    if let Some(cb) = self.clipboard.as_mut() {
                        let _ = cb.set_text(u.clone());
                        self.last_clip = u;
                    }
                    self.toast(t(Key::ToastUrlCopied));
                }
            }
        }
    }
}

fn playlist_mode_label(m: PlaylistMode) -> &'static str {
    t(match m {
        PlaylistMode::Expand => Key::PlModeExpand,
        PlaylistMode::Single => Key::PlModeSingle,
        PlaylistMode::VideoOnly => Key::PlModeVideoOnly,
    })
}

fn playlist_mode_hint(m: PlaylistMode) -> &'static str {
    t(match m {
        PlaylistMode::Expand => Key::PlHintExpand,
        PlaylistMode::Single => Key::PlHintSingle,
        PlaylistMode::VideoOnly => Key::PlHintVideoOnly,
    })
}

fn job_row(ui: &mut egui::Ui, j: &mut Job, acts: &mut Vec<Act>) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(status_color(j.status)));
                let title = if j.title.is_empty() {
                    j.url.clone()
                } else {
                    j.title.clone()
                };
                ui.label(RichText::new(truncate(&title, 90)).strong())
                    .on_hover_text(&title);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(j.status.label())
                            .small()
                            .color(status_color(j.status)),
                    );
                    if let Some((i, n)) = j.item {
                        ui.label(RichText::new(format!("{i}/{n}")).small().weak());
                    }
                    if j.kind == JobKind::Resolve {
                        ui.label(RichText::new(t(Key::JobBadgeList)).small().weak());
                    }
                    let stamp = j.finished_at.unwrap_or(j.added);
                    ui.label(
                        RichText::new(stamp.format("%H:%M").to_string())
                            .small()
                            .weak(),
                    )
                    .on_hover_text(tf(
                        Key::JobAddedAt,
                        &[&j.added.format("%Y-%m-%d %H:%M:%S").to_string()],
                    ));
                });
            });

            // 진행률
            if matches!(j.status, Status::Running | Status::Paused) || j.progress > 0.0 {
                let text = if j.total > 0 {
                    format!(
                        "{:.1}%  ·  {} / {}",
                        j.progress * 100.0,
                        fmt_bytes(j.downloaded),
                        fmt_bytes(j.total)
                    )
                } else {
                    format!("{:.1}%", j.progress * 100.0)
                };
                ui.add(
                    egui::ProgressBar::new(j.progress)
                        .desired_height(14.0)
                        .text(RichText::new(text).small()),
                );
            }

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if j.status == Status::Running {
                    if j.speed > 0.0 {
                        ui.label(RichText::new(fmt_speed(j.speed)).small().weak());
                    }
                    if j.eta > 0 {
                        ui.label(
                            RichText::new(tf(Key::JobEta, &[&fmt_eta(j.eta)]))
                                .small()
                                .weak(),
                        );
                    }
                }
                if !j.stage.is_empty() {
                    ui.label(RichText::new(&j.stage).small().weak());
                }
                if let Some(e) = &j.error {
                    ui.label(
                        RichText::new(truncate(e, 160))
                            .small()
                            .color(Color32::from_rgb(220, 110, 110)),
                    )
                    .on_hover_text(e);
                }
            });

            // 버튼 줄
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                match j.status {
                    Status::Running => {
                        if ui.small_button(format!("⏸ {}", t(Key::BtnPause))).clicked() {
                            acts.push(Act::Pause(j.id));
                        }
                        if ui
                            .small_button(format!("⏹ {}", t(Key::BtnCancel)))
                            .clicked()
                        {
                            acts.push(Act::Cancel(j.id));
                        }
                    }
                    Status::Queued => {
                        if ui.small_button(format!("⏸ {}", t(Key::BtnHold))).clicked() {
                            acts.push(Act::Pause(j.id));
                        }
                    }
                    Status::Paused => {
                        if ui
                            .small_button(format!("▶ {}", t(Key::BtnResume)))
                            .clicked()
                        {
                            acts.push(Act::Start(j.id));
                        }
                    }
                    Status::Failed | Status::Canceled => {
                        if ui.small_button(format!("↻ {}", t(Key::BtnRetry))).clicked() {
                            acts.push(Act::Start(j.id));
                        }
                    }
                    Status::Done => {
                        if let Some(p) = j.filepath.clone() {
                            if ui.small_button(format!("▶ {}", t(Key::BtnOpen))).clicked() {
                                acts.push(Act::OpenFile(p.clone()));
                            }
                            if ui
                                .small_button(format!("📂 {}", t(Key::BtnReveal)))
                                .clicked()
                            {
                                acts.push(Act::Reveal(p));
                            }
                        }
                    }
                }

                if ui
                    .small_button(format!("🔗 {}", t(Key::BtnCopyUrl)))
                    .clicked()
                {
                    acts.push(Act::CopyUrl(j.url.clone()));
                }
                let log_label = if j.show_log {
                    t(Key::BtnHideLog)
                } else {
                    t(Key::BtnLog)
                };
                if ui.small_button(log_label).clicked() {
                    j.show_log = !j.show_log;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("✖")
                        .on_hover_text(t(Key::TipRemoveFromList))
                        .clicked()
                    {
                        acts.push(Act::Remove(j.id));
                    }
                });
            });

            if j.show_log {
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .id_salt(("log", j.id))
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in j.log.iter() {
                            ui.label(RichText::new(l).monospace().small());
                        }
                    });
            }
        });
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

// ─────────────────────────────────────────────────────────────
// 설정 창
// ─────────────────────────────────────────────────────────────

impl App {
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let mut dirty = false;
        let mut theme_changed = None;
        let mut lang_changed: Option<Option<Lang>> = None;
        let mut do_install_yt = false;
        let mut do_install_ff = false;
        let mut do_check = false;
        let mut reprobe = false;

        egui::Window::new(t(Key::BtnSettings))
            .open(&mut open)
            .default_width(560.0)
            .max_height(640.0)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut st = self.settings.lock().unwrap();

                    // ── 저장 위치 ─────────────────────────
                    ui.heading(t(Key::HdrSave));
                    egui::Grid::new("g_save")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(t(Key::LblSaveFolder));
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(st.download_dir.display().to_string()).small(),
                                );
                                if ui.small_button(t(Key::BtnChange)).clicked() {
                                    if let Some(p) = rfd::FileDialog::new()
                                        .set_directory(&st.download_dir)
                                        .pick_folder()
                                    {
                                        st.download_dir = p;
                                        dirty = true;
                                    }
                                }
                            });
                            ui.end_row();

                            ui.label(t(Key::LblSeparateDirs));
                            ui.vertical(|ui| {
                                dirty |= ui
                                    .checkbox(&mut st.separate_media_dirs, t(Key::ChkSeparateDirs))
                                    .changed();
                                if st.separate_media_dirs {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(t(Key::LblVideo)).small().weak());
                                        dirty |= ui
                                            .add(
                                                egui::TextEdit::singleline(&mut st.video_subdir)
                                                    .desired_width(90.0),
                                            )
                                            .changed();
                                        ui.label(RichText::new(t(Key::LblAudio)).small().weak());
                                        dirty |= ui
                                            .add(
                                                egui::TextEdit::singleline(&mut st.audio_subdir)
                                                    .desired_width(90.0),
                                            )
                                            .changed();
                                    });
                                    ui.label(
                                        RichText::new(tf(
                                            Key::LblCurrentTarget,
                                            &[&st.target_dir().display().to_string()],
                                        ))
                                        .small()
                                        .weak(),
                                    );
                                }
                            });
                            ui.end_row();

                            ui.label(t(Key::LblFilenameTemplate));
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut st.output_template)
                                        .desired_width(320.0),
                                )
                                .on_hover_text(t(Key::TipFilenameTemplate))
                                .changed()
                            {
                                dirty = true;
                            }
                            ui.end_row();

                            ui.label("");
                            ui.vertical(|ui| {
                                dirty |= ui
                                    .checkbox(
                                        &mut st.restrict_filenames,
                                        t(Key::ChkRestrictFilenames),
                                    )
                                    .changed();
                                dirty |= ui
                                    .checkbox(&mut st.use_archive, t(Key::ChkUseArchive))
                                    .changed();
                            });
                            ui.end_row();
                        });

                    ui.add_space(10.0);
                    ui.heading(t(Key::HdrQualityFormat));
                    egui::Grid::new("g_fmt")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(t(Key::LblDownload));
                            ui.horizontal(|ui| {
                                for m in [Mode::Video, Mode::Audio] {
                                    dirty |= ui
                                        .selectable_value(&mut st.mode, m, mode_label(m))
                                        .changed();
                                }
                            });
                            ui.end_row();

                            if st.mode == Mode::Video {
                                ui.label(t(Key::LblMaxQuality));
                                egui::ComboBox::from_id_salt("q")
                                    .width(160.0)
                                    .selected_text(height_label(st.max_height))
                                    .show_ui(ui, |ui| {
                                        for h in [0u32, 2160, 1440, 1080, 720, 480, 360] {
                                            dirty |= ui
                                                .selectable_value(
                                                    &mut st.max_height,
                                                    h,
                                                    height_label(h),
                                                )
                                                .changed();
                                        }
                                    });
                                ui.end_row();

                                ui.label(t(Key::LblMaxFps));
                                egui::ComboBox::from_id_salt("fps")
                                    .width(160.0)
                                    .selected_text(if st.max_fps == 0 {
                                        t(Key::Unlimited).to_string()
                                    } else {
                                        format!("{} fps", st.max_fps)
                                    })
                                    .show_ui(ui, |ui| {
                                        for f in [0u32, 60, 30] {
                                            let l = if f == 0 {
                                                t(Key::Unlimited).to_string()
                                            } else {
                                                format!("{f} fps")
                                            };
                                            dirty |= ui
                                                .selectable_value(&mut st.max_fps, f, l)
                                                .changed();
                                        }
                                    });
                                ui.end_row();

                                ui.label(t(Key::LblPreferCodec));
                                egui::ComboBox::from_id_salt("codec")
                                    .width(160.0)
                                    .selected_text(codec_label(&st.prefer_codec))
                                    .show_ui(ui, |ui| {
                                        for c in ["auto", "h264", "av1", "vp9"] {
                                            let mut cur = st.prefer_codec.clone();
                                            if ui
                                                .selectable_value(
                                                    &mut cur,
                                                    c.to_string(),
                                                    codec_label(c),
                                                )
                                                .changed()
                                            {
                                                st.prefer_codec = cur;
                                                dirty = true;
                                            }
                                        }
                                    });
                                ui.end_row();

                                ui.label(t(Key::LblContainer));
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt("cont")
                                        .width(110.0)
                                        .selected_text(if st.container == "auto" {
                                            t(Key::Auto).to_string()
                                        } else {
                                            st.container.clone()
                                        })
                                        .show_ui(ui, |ui| {
                                            for c in ["auto", "mp4", "mkv", "webm"] {
                                                let label =
                                                    if c == "auto" { t(Key::Auto) } else { c };
                                                let mut cur = st.container.clone();
                                                if ui
                                                    .selectable_value(
                                                        &mut cur,
                                                        c.to_string(),
                                                        label,
                                                    )
                                                    .changed()
                                                {
                                                    st.container = cur;
                                                    dirty = true;
                                                }
                                            }
                                        });
                                    dirty |= ui
                                        .checkbox(&mut st.force_remux, t(Key::ChkForceRemux))
                                        .on_hover_text(t(Key::TipForceRemux))
                                        .changed();
                                });
                                ui.end_row();
                            } else {
                                ui.label(t(Key::LblAudioFormat));
                                egui::ComboBox::from_id_salt("af")
                                    .width(160.0)
                                    .selected_text(&st.audio_format)
                                    .show_ui(ui, |ui| {
                                        for f in AUDIO_FORMATS {
                                            let mut cur = st.audio_format.clone();
                                            if ui
                                                .selectable_value(&mut cur, f.to_string(), f)
                                                .changed()
                                            {
                                                st.audio_format = cur;
                                                dirty = true;
                                            }
                                        }
                                    });
                                ui.end_row();

                                ui.label(t(Key::LblAudioQuality));
                                egui::ComboBox::from_id_salt("ab")
                                    .width(160.0)
                                    .selected_text(bitrate_label(&st.audio_bitrate))
                                    .show_ui(ui, |ui| {
                                        for b in AUDIO_BITRATES {
                                            let mut cur = st.audio_bitrate.clone();
                                            if ui
                                                .selectable_value(
                                                    &mut cur,
                                                    b.to_string(),
                                                    bitrate_label(b),
                                                )
                                                .changed()
                                            {
                                                st.audio_bitrate = cur;
                                                dirty = true;
                                            }
                                        }
                                    });
                                ui.end_row();
                            }
                        });

                    ui.add_space(10.0);
                    ui.heading(t(Key::HdrExtras));
                    ui.horizontal_wrapped(|ui| {
                        dirty |= ui
                            .checkbox(&mut st.embed_metadata, t(Key::ChkEmbedMetadata))
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.embed_thumbnail, t(Key::ChkEmbedThumbnail))
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.embed_chapters, t(Key::ChkEmbedChapters))
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.sponsorblock_remove, t(Key::ChkSponsorBlock))
                            .changed();
                    });
                    if st.mode == Mode::Video {
                        ui.horizontal_wrapped(|ui| {
                            dirty |= ui
                                .checkbox(&mut st.write_subs, t(Key::ChkWriteSubs))
                                .changed();
                            dirty |= ui
                                .checkbox(&mut st.auto_subs, t(Key::ChkAutoSubs))
                                .changed();
                            dirty |= ui
                                .checkbox(&mut st.embed_subs, t(Key::ChkEmbedSubs))
                                .changed();
                            ui.label(t(Key::LblSubLangs));
                            dirty |= ui
                                .add(
                                    egui::TextEdit::singleline(&mut st.sub_langs)
                                        .desired_width(110.0),
                                )
                                .on_hover_text(t(Key::TipSubLangs))
                                .changed();
                        });
                    }

                    ui.add_space(10.0);
                    ui.heading(t(Key::LblPlaylist));
                    egui::Grid::new("g_pl")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(t(Key::LblPlDefaultMode));
                            egui::ComboBox::from_id_salt("plm")
                                .width(180.0)
                                .selected_text(playlist_mode_label(st.playlist_mode))
                                .show_ui(ui, |ui| {
                                    for m in [
                                        PlaylistMode::Expand,
                                        PlaylistMode::Single,
                                        PlaylistMode::VideoOnly,
                                    ] {
                                        dirty |= ui
                                            .selectable_value(
                                                &mut st.playlist_mode,
                                                m,
                                                playlist_mode_label(m),
                                            )
                                            .changed();
                                    }
                                });
                            ui.end_row();

                            ui.label(t(Key::LblPlItems));
                            dirty |= ui
                                .add(
                                    egui::TextEdit::singleline(&mut st.playlist_items)
                                        .desired_width(180.0)
                                        .hint_text(t(Key::HintPlItems)),
                                )
                                .on_hover_text(t(Key::TipPlItems))
                                .changed();
                            ui.end_row();

                            ui.label(t(Key::LblPlExpandLimit));
                            dirty |= ui
                                .add(
                                    egui::DragValue::new(&mut st.playlist_expand_limit)
                                        .range(0..=5000),
                                )
                                .on_hover_text(t(Key::TipPlExpandLimit))
                                .changed();
                            ui.end_row();

                            ui.label("");
                            dirty |= ui
                                .checkbox(&mut st.playlist_reverse, t(Key::ChkPlReverse))
                                .changed();
                            ui.end_row();
                        });

                    ui.add_space(10.0);
                    ui.heading(t(Key::HdrNetwork));
                    egui::Grid::new("g_net")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(t(Key::LblMaxConcurrent));
                            dirty |= ui
                                .add(egui::Slider::new(&mut st.max_concurrent_downloads, 1..=8))
                                .changed();
                            ui.end_row();

                            ui.label(t(Key::LblConcurrentFragments));
                            dirty |= ui
                                .add(egui::Slider::new(&mut st.concurrent_fragments, 1..=16))
                                .on_hover_text(t(Key::TipConcurrentFragments))
                                .changed();
                            ui.end_row();

                            ui.label(t(Key::LblRateLimit));
                            dirty |= ui
                                .add(
                                    egui::TextEdit::singleline(&mut st.rate_limit)
                                        .desired_width(120.0)
                                        .hint_text(t(Key::HintRateLimit)),
                                )
                                .changed();
                            ui.end_row();

                            ui.label(t(Key::LblRetries));
                            dirty |= ui
                                .add(egui::DragValue::new(&mut st.retries).range(0..=100))
                                .changed();
                            ui.end_row();

                            ui.label(t(Key::LblProxy));
                            dirty |= ui
                                .add(
                                    egui::TextEdit::singleline(&mut st.proxy)
                                        .desired_width(240.0)
                                        .hint_text("http://host:port / socks5://host:port"),
                                )
                                .changed();
                            ui.end_row();

                            ui.label(t(Key::LblBrowserCookies));
                            egui::ComboBox::from_id_salt("cookies")
                                .width(160.0)
                                .selected_text(if st.cookies_from_browser.is_empty() {
                                    t(Key::OptDisabled).to_string()
                                } else {
                                    st.cookies_from_browser.clone()
                                })
                                .show_ui(ui, |ui| {
                                    for b in [
                                        "", "chrome", "edge", "firefox", "safari", "brave",
                                        "whale", "opera",
                                    ] {
                                        let label =
                                            if b.is_empty() { t(Key::OptDisabled) } else { b };
                                        let mut cur = st.cookies_from_browser.clone();
                                        if ui
                                            .selectable_value(&mut cur, b.to_string(), label)
                                            .changed()
                                        {
                                            st.cookies_from_browser = cur;
                                            dirty = true;
                                        }
                                    }
                                });
                            ui.end_row();
                        });

                    ui.add_space(10.0);
                    ui.heading(t(Key::HdrBehavior));
                    ui.vertical(|ui| {
                        dirty |= ui
                            .checkbox(&mut st.paste_to_download, t(Key::ChkPasteToDownload))
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.clipboard_watch, t(Key::ChkClipboardWatch))
                            .on_hover_text(t(Key::TipClipboardWatch))
                            .changed();
                        ui.horizontal_wrapped(|ui| {
                            dirty |= ui
                                .checkbox(&mut st.auto_start, t(Key::ChkAutoStart))
                                .changed();
                            dirty |= ui
                                .checkbox(&mut st.skip_duplicates, t(Key::ChkSkipDuplicates))
                                .changed();
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label(t(Key::LblTheme));
                        for (pref, key) in [
                            (ThemePref::System, Key::ThemeSystem),
                            (ThemePref::Light, Key::ThemeLight),
                            (ThemePref::Dark, Key::ThemeDark),
                        ] {
                            if ui.selectable_value(&mut st.theme, pref, t(key)).changed() {
                                dirty = true;
                                theme_changed = Some(pref);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(t(Key::LblLanguage));
                        let cur = st.language;
                        let mut sel = cur;
                        egui::ComboBox::from_id_salt("lang")
                            .width(170.0)
                            .selected_text(match cur {
                                Some(l) => l.native_name(),
                                None => t(Key::LangSystem),
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut sel, None, t(Key::LangSystem));
                                for l in i18n::ALL_LANGS {
                                    ui.selectable_value(&mut sel, Some(l), l.native_name());
                                }
                            });
                        if sel != cur {
                            st.language = sel;
                            dirty = true;
                            // 폰트까지 바꿔야 하므로 창을 그린 뒤에 한 번에 반영한다.
                            lang_changed = Some(sel);
                        }
                    })
                    .response
                    .on_hover_text(t(Key::TipLanguage));

                    ui.add_space(10.0);
                    ui.heading(t(Key::HdrTools));
                    let tool_state = self.tools.lock().unwrap().clone();

                    ui.horizontal_wrapped(|ui| {
                        dirty |= ui
                            .checkbox(&mut st.auto_update_check, t(Key::ChkAutoUpdateCheck))
                            .on_hover_text(t(Key::TipAutoUpdateCheck))
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.auto_update_tools, t(Key::ChkAutoUpdateTools))
                            .on_hover_text(t(Key::TipAutoUpdateTools))
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.auto_install_ffmpeg, t(Key::ChkAutoInstallFfmpeg))
                            .changed();
                    });
                    ui.add_space(4.0);

                    egui::Grid::new("g_bin")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            // ── yt-dlp ──────────────────────
                            ui.label(t(Key::LblYtdlpBinary));
                            ui.horizontal(|ui| {
                                for (m, key) in [
                                    (BinMode::Managed, Key::BinManaged),
                                    (BinMode::System, Key::BinSystem),
                                    (BinMode::Custom, Key::BinCustom),
                                ] {
                                    if ui.selectable_value(&mut st.bin_mode, m, t(key)).changed() {
                                        dirty = true;
                                        reprobe = true;
                                    }
                                }
                            });
                            ui.end_row();

                            if st.bin_mode == BinMode::Custom {
                                ui.label(t(Key::LblYtdlpPath));
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut st.ytdlp_custom_path)
                                                .desired_width(280.0),
                                        )
                                        .changed()
                                    {
                                        dirty = true;
                                        reprobe = true;
                                    }
                                    if ui.small_button(t(Key::BtnBrowse)).clicked() {
                                        if let Some(p) = rfd::FileDialog::new().pick_file() {
                                            st.ytdlp_custom_path = p.display().to_string();
                                            dirty = true;
                                            reprobe = true;
                                        }
                                    }
                                });
                                ui.end_row();
                            }

                            ui.label(t(Key::LblYtdlpStatus));
                            ui.vertical(|ui| {
                                tool_status_line(ui, &tool_state.ytdlp);
                                ui.horizontal(|ui| {
                                    let label = if st.bin_mode == BinMode::Managed {
                                        t(Key::BtnInstallUpdateNow)
                                    } else {
                                        t(Key::BtnRunYtdlpU)
                                    };
                                    if ui
                                        .add_enabled(
                                            !tool_state.ytdlp.busy,
                                            egui::Button::new(label),
                                        )
                                        .clicked()
                                    {
                                        do_install_yt = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            !tool_state.ytdlp.busy,
                                            egui::Button::new(t(Key::BtnCheckLatest)),
                                        )
                                        .clicked()
                                    {
                                        do_check = true;
                                    }
                                });
                            });
                            ui.end_row();

                            // ── ffmpeg ──────────────────────
                            ui.label(t(Key::LblFfmpegPath));
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut st.ffmpeg_custom_path)
                                            .desired_width(280.0)
                                            .hint_text(t(Key::HintFfmpegPath)),
                                    )
                                    .changed()
                                {
                                    dirty = true;
                                    reprobe = true;
                                }
                                if ui.small_button(t(Key::BtnBrowse)).clicked() {
                                    if let Some(p) = rfd::FileDialog::new().pick_file() {
                                        st.ffmpeg_custom_path = p.display().to_string();
                                        dirty = true;
                                        reprobe = true;
                                    }
                                }
                            });
                            ui.end_row();

                            ui.label(t(Key::LblFfmpegStatus));
                            ui.vertical(|ui| {
                                tool_status_line(ui, &tool_state.ffmpeg);
                                if tool_state.ffmpeg.local.is_none() {
                                    if let Some(bad) = tools::broken_ffmpeg(&st) {
                                        ui.label(
                                            RichText::new(tf(
                                                Key::MsgFfmpegBroken,
                                                &[&bad.display().to_string()],
                                            ))
                                            .small()
                                            .color(Color32::from_rgb(220, 110, 110)),
                                        );
                                    }
                                    ui.label(
                                        RichText::new(t(Key::MsgFfmpegMissingWarn))
                                            .small()
                                            .color(Color32::from_rgb(220, 150, 60)),
                                    );
                                }
                                if ui
                                    .add_enabled(
                                        !tool_state.ffmpeg.busy,
                                        egui::Button::new(t(Key::BtnInstallUpdateNow)),
                                    )
                                    .on_hover_text(t(Key::TipInstallFfmpeg))
                                    .clicked()
                                {
                                    do_install_ff = true;
                                }
                            });
                            ui.end_row();

                            ui.label(t(Key::LblInstallDir));
                            ui.label(
                                RichText::new(crate::tools::managed_dir().display().to_string())
                                    .small()
                                    .weak(),
                            );
                            ui.end_row();
                        });

                    ui.add_space(10.0);
                    ui.collapsing(t(Key::HdrAdvanced), |ui| {
                        dirty |= ui
                            .checkbox(&mut st.ignore_yt_dlp_config, t(Key::ChkIgnoreConfig))
                            .on_hover_text(t(Key::TipIgnoreConfig))
                            .changed();
                        ui.label(t(Key::LblExtraArgs));
                        dirty |= ui
                            .add(
                                egui::TextEdit::multiline(&mut st.extra_args)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY)
                                    .hint_text(t(Key::HintExtraArgs)),
                            )
                            .changed();
                        ui.label(
                            RichText::new(tf(
                                Key::LblConfigFile,
                                &[&crate::config::config_path().display().to_string()],
                            ))
                            .small()
                            .weak(),
                        );
                    });
                });
            });

        if dirty {
            self.save_settings();
        }
        if let Some(pref) = theme_changed {
            apply_theme(ctx, pref);
        }
        if let Some(pref) = lang_changed {
            crate::util::install_fonts(ctx, i18n::apply_pref(pref));
        }
        if reprobe {
            self.refresh_tools();
        }
        if do_check {
            tools::spawn_startup_check(
                self.tools.clone(),
                self.settings_snapshot(),
                ctx.clone(),
                true,
            );
        }
        if do_install_yt {
            tools::spawn_install_ytdlp(self.tools.clone(), self.settings_snapshot(), ctx.clone());
        }
        if do_install_ff {
            tools::spawn_install_ffmpeg(self.tools.clone(), ctx.clone());
        }
        self.show_settings = open;
    }
}

/// 설정 창에서 도구의 버전/메시지/진행률을 한 줄로 보여 준다.
fn tool_status_line(ui: &mut egui::Ui, st: &ToolState) {
    ui.horizontal(|ui| match &st.local {
        Some(v) => {
            ui.label(RichText::new(tf(Key::ToolInstalled, &[v])).small());
            if let Some(l) = st.latest_pretty() {
                ui.label(RichText::new(tf(Key::ToolLatestIs, &[&l])).small().weak());
            }
            if !st.managed {
                ui.label(RichText::new(t(Key::ToolSystemCopy)).small().weak());
            }
        }
        None => {
            ui.label(
                RichText::new(t(Key::ToolNotInstalled))
                    .small()
                    .color(Color32::from_rgb(220, 90, 90)),
            );
        }
    });
    if let Some((got, total, label)) = &st.progress {
        let f = if *total > 0 {
            *got as f32 / *total as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(f)
                .desired_height(10.0)
                .text(RichText::new(format!("{label} {}", fmt_bytes(*got))).small()),
        );
    }
    if !st.message.is_empty() {
        ui.label(RichText::new(&st.message).small().weak());
    }
}

pub const AUDIO_FORMATS: [&str; 6] = ["best", "mp3", "m4a", "opus", "flac", "wav"];
pub const AUDIO_BITRATES: [&str; 7] = ["best", "320K", "256K", "192K", "128K", "96K", "64K"];

/// URL 입력창의 고정 id — 전역 붙여넣기 처리에서 포커스를 구분하는 데 쓴다.
fn url_input_id() -> egui::Id {
    egui::Id::new("stratos_url_input")
}

fn mode_label(m: Mode) -> &'static str {
    t(match m {
        Mode::Video => Key::LblVideo,
        Mode::Audio => Key::LblAudio,
    })
}

fn bitrate_label(v: &str) -> String {
    if v == "best" {
        t(Key::AudioBest).to_string()
    } else {
        format!("{}kbps", v.trim_end_matches(['K', 'k']))
    }
}

fn height_label(h: u32) -> String {
    match h {
        0 => t(Key::QualityBest).to_string(),
        2160 => "2160p (4K)".to_string(),
        1440 => "1440p (2K)".to_string(),
        v => format!("{v}p"),
    }
}

fn codec_label(c: &str) -> &'static str {
    match c {
        "h264" => t(Key::CodecH264),
        "av1" => t(Key::CodecAv1),
        "vp9" => "VP9",
        _ => t(Key::Auto),
    }
}
