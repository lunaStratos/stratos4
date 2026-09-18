//! egui 기반 UI

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::config::{BinMode, Mode, PlaylistMode, Settings, ThemePref};
use crate::engine::{Engine, Job, JobKind, Status};
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
        crate::util::install_korean_font(&cc.egui_ctx);
        cc.egui_ctx.set_pixels_per_point(1.15);

        let mut settings = Settings::load();
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
                self.toast("URL 을 찾지 못했습니다.");
            }
            return;
        }
        let n = self.engine.add_many(&urls, self.add_mode);
        if n == 0 {
            self.toast("이미 대기열에 있는 URL 입니다.");
        } else {
            self.toast(format!("{n}개 추가됨"));
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
            self.toast(format!("클립보드에서 {n}개 추가됨"));
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
            format!("{name} 갱신 중{pct}"),
            Color32::from_rgb(210, 175, 70),
            st.message.clone(),
        )
    } else {
        match &st.local {
            Some(v) if st.update_available() => (
                format!("{name} {v} ↑"),
                Color32::from_rgb(220, 150, 60),
                format!(
                    "새 버전 {} 사용 가능",
                    st.latest_pretty().unwrap_or_default()
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
                format!("{name} 없음"),
                Color32::from_rgb(220, 90, 90),
                if st.message.is_empty() {
                    format!("{name} 이(가) 설치되어 있지 않습니다. 설정에서 설치하세요.")
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
                ui.label(RichText::new("yt-dlp 기반 다운로더").weak().small());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙  설정").clicked() {
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
                t.ytdlp.progress.clone().or_else(|| t.ffmpeg.progress.clone())
            };
            if let Some((got, total, label)) = dl {
                let frac = if total > 0 { got as f32 / total as f32 } else { 0.0 };
                let text = if total > 0 {
                    format!("{label} 내려받는 중  {} / {}", fmt_bytes(got), fmt_bytes(total))
                } else {
                    format!("{label} 내려받는 중  {}", fmt_bytes(got))
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
                        .hint_text("여기에 URL 을 붙여넣으세요 (Ctrl/Cmd+V) — 플레이리스트·채널 주소도 가능"),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                if ui.add_sized([56.0, 28.0], egui::Button::new("추가")).clicked() || enter {
                    let text = std::mem::take(&mut self.input);
                    if !text.trim().is_empty() {
                        self.add_from_text(&text);
                    }
                    resp.request_focus();
                }
                if ui
                    .add_sized([84.0, 28.0], egui::Button::new("📋 붙여넣기"))
                    .on_hover_text("클립보드 내용을 바로 대기열에 추가")
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

                ui.label(RichText::new("다운로드").weak());
                egui::ComboBox::from_id_salt("quick_mode")
                    .width(88.0)
                    .selected_text(mode_label(st.mode))
                    .show_ui(ui, |ui| {
                        for m in [Mode::Video, Mode::Audio] {
                            changed |= ui.selectable_value(&mut st.mode, m, mode_label(m)).changed();
                        }
                    });

                ui.add_space(8.0);
                ui.label(RichText::new("화질").weak());
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
                    ui.label(RichText::new("포맷").weak());
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
                ui.label(RichText::new("플레이리스트").weak());
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
                    .checkbox(&mut st.clipboard_watch, "복사 즉시 추가")
                    .on_hover_text("켜면 붙여넣지 않아도 URL 을 복사하는 순간 대기열에 들어갑니다.")
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
                    RichText::new(format!("진행 {r} · 대기 {q} · 완료 {d} · 실패 {f}")).small(),
                );

                ui.separator();
                let dir = self.settings.lock().unwrap().target_dir();
                if ui
                    .link(RichText::new(format!("📁 {}", dir.display())).small())
                    .on_hover_text("저장 폴더 열기")
                    .clicked()
                {
                    let _ = open::that_detached(&dir);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("완료 항목 정리").clicked() {
                        self.engine.clear_finished();
                    }
                    if ui.button("⏸ 전체 일시정지").clicked() {
                        self.engine.pause_all();
                    }
                    if ui.button("▶ 전체 시작").clicked() {
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
                    ui.label(
                        RichText::new(
                            "이 창에서 Ctrl/Cmd+V 로 URL 을 붙여넣으면 바로 다운로드가 시작됩니다.\n\
                             플레이리스트·채널 주소도 그대로 붙여넣으세요.",
                        )
                        .weak(),
                    );
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
                    self.toast("URL 을 복사했습니다.");
                }
            }
        }
    }
}

fn playlist_mode_label(m: PlaylistMode) -> &'static str {
    match m {
        PlaylistMode::Expand => "항목별로 펼치기",
        PlaylistMode::Single => "한 작업으로 일괄",
        PlaylistMode::VideoOnly => "이 영상만",
    }
}

fn playlist_mode_hint(m: PlaylistMode) -> &'static str {
    match m {
        PlaylistMode::Expand => {
            "목록을 읽어 항목마다 작업을 만듭니다 (개별 진행률·재시도·동시 다운로드)"
        }
        PlaylistMode::Single => "yt-dlp 가 목록 전체를 순서대로 처리합니다",
        PlaylistMode::VideoOnly => "목록 파라미터를 무시하고 해당 영상 하나만 받습니다",
    }
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
                        ui.label(RichText::new("목록").small().weak());
                    }
                    let stamp = j.finished_at.unwrap_or(j.added);
                    ui.label(
                        RichText::new(stamp.format("%H:%M").to_string())
                            .small()
                            .weak(),
                    )
                    .on_hover_text(format!("추가 {}", j.added.format("%Y-%m-%d %H:%M:%S")));
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
                            RichText::new(format!("남은 시간 {}", fmt_eta(j.eta)))
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
                        if ui.small_button("⏸ 일시정지").clicked() {
                            acts.push(Act::Pause(j.id));
                        }
                        if ui.small_button("⏹ 취소").clicked() {
                            acts.push(Act::Cancel(j.id));
                        }
                    }
                    Status::Queued => {
                        if ui.small_button("⏸ 보류").clicked() {
                            acts.push(Act::Pause(j.id));
                        }
                    }
                    Status::Paused => {
                        if ui.small_button("▶ 이어받기").clicked() {
                            acts.push(Act::Start(j.id));
                        }
                    }
                    Status::Failed | Status::Canceled => {
                        if ui.small_button("↻ 다시 시도").clicked() {
                            acts.push(Act::Start(j.id));
                        }
                    }
                    Status::Done => {
                        if let Some(p) = j.filepath.clone() {
                            if ui.small_button("▶ 열기").clicked() {
                                acts.push(Act::OpenFile(p.clone()));
                            }
                            if ui.small_button("📂 위치 보기").clicked() {
                                acts.push(Act::Reveal(p));
                            }
                        }
                    }
                }

                if ui.small_button("🔗 URL 복사").clicked() {
                    acts.push(Act::CopyUrl(j.url.clone()));
                }
                let log_label = if j.show_log {
                    "로그 숨기기"
                } else {
                    "로그"
                };
                if ui.small_button(log_label).clicked() {
                    j.show_log = !j.show_log;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("✖")
                        .on_hover_text("목록에서 제거")
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
        let mut do_install_yt = false;
        let mut do_install_ff = false;
        let mut do_check = false;
        let mut reprobe = false;

        egui::Window::new("설정")
            .open(&mut open)
            .default_width(560.0)
            .max_height(640.0)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut st = self.settings.lock().unwrap();

                    // ── 저장 위치 ─────────────────────────
                    ui.heading("저장");
                    egui::Grid::new("g_save").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("저장 폴더");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(st.download_dir.display().to_string()).small());
                            if ui.small_button("변경").clicked() {
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

                        ui.label("폴더 분리");
                        ui.vertical(|ui| {
                            dirty |= ui
                                .checkbox(
                                    &mut st.separate_media_dirs,
                                    "영상과 오디오를 하위 폴더로 나눠 저장",
                                )
                                .changed();
                            if st.separate_media_dirs {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("영상").small().weak());
                                    dirty |= ui
                                        .add(
                                            egui::TextEdit::singleline(&mut st.video_subdir)
                                                .desired_width(90.0),
                                        )
                                        .changed();
                                    ui.label(RichText::new("오디오").small().weak());
                                    dirty |= ui
                                        .add(
                                            egui::TextEdit::singleline(&mut st.audio_subdir)
                                                .desired_width(90.0),
                                        )
                                        .changed();
                                });
                                ui.label(
                                    RichText::new(format!(
                                        "지금 저장 위치: {}",
                                        st.target_dir().display()
                                    ))
                                    .small()
                                    .weak(),
                                );
                            }
                        });
                        ui.end_row();

                        ui.label("파일명 형식");
                        if ui
                            .add(egui::TextEdit::singleline(&mut st.output_template).desired_width(320.0))
                            .on_hover_text("yt-dlp 출력 템플릿. 예: %(title)s.%(ext)s")
                            .changed()
                        {
                            dirty = true;
                        }
                        ui.end_row();

                        ui.label("");
                        ui.vertical(|ui| {
                            dirty |= ui
                                .checkbox(&mut st.restrict_filenames, "파일명을 ASCII 로 제한")
                                .changed();
                            dirty |= ui
                                .checkbox(&mut st.use_archive, "이미 받은 항목 건너뛰기 (아카이브 기록)")
                                .changed();
                        });
                        ui.end_row();
                    });

                    ui.add_space(10.0);
                    ui.heading("화질 / 포맷");
                    egui::Grid::new("g_fmt").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("다운로드");
                        ui.horizontal(|ui| {
                            for m in [Mode::Video, Mode::Audio] {
                                dirty |= ui.selectable_value(&mut st.mode, m, mode_label(m)).changed();
                            }
                        });
                        ui.end_row();

                        if st.mode == Mode::Video {
                            ui.label("최대 화질");
                            egui::ComboBox::from_id_salt("q")
                                .width(160.0)
                                .selected_text(height_label(st.max_height))
                                .show_ui(ui, |ui| {
                                    for h in [0u32, 2160, 1440, 1080, 720, 480, 360] {
                                        dirty |= ui
                                            .selectable_value(&mut st.max_height, h, height_label(h))
                                            .changed();
                                    }
                                });
                            ui.end_row();

                            ui.label("최대 프레임");
                            egui::ComboBox::from_id_salt("fps")
                                .width(160.0)
                                .selected_text(if st.max_fps == 0 {
                                    "제한 없음".to_string()
                                } else {
                                    format!("{} fps", st.max_fps)
                                })
                                .show_ui(ui, |ui| {
                                    for f in [0u32, 60, 30] {
                                        let l = if f == 0 {
                                            "제한 없음".to_string()
                                        } else {
                                            format!("{f} fps")
                                        };
                                        dirty |= ui.selectable_value(&mut st.max_fps, f, l).changed();
                                    }
                                });
                            ui.end_row();

                            ui.label("코덱 선호");
                            egui::ComboBox::from_id_salt("codec")
                                .width(160.0)
                                .selected_text(codec_label(&st.prefer_codec))
                                .show_ui(ui, |ui| {
                                    for c in ["auto", "h264", "av1", "vp9"] {
                                        let mut cur = st.prefer_codec.clone();
                                        if ui.selectable_value(&mut cur, c.to_string(), codec_label(c)).changed() {
                                            st.prefer_codec = cur;
                                            dirty = true;
                                        }
                                    }
                                });
                            ui.end_row();

                            ui.label("컨테이너");
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("cont")
                                    .width(110.0)
                                    .selected_text(if st.container == "auto" {
                                        "자동".to_string()
                                    } else {
                                        st.container.clone()
                                    })
                                    .show_ui(ui, |ui| {
                                        for c in ["auto", "mp4", "mkv", "webm"] {
                                            let label = if c == "auto" { "자동" } else { c };
                                            let mut cur = st.container.clone();
                                            if ui.selectable_value(&mut cur, c.to_string(), label).changed() {
                                                st.container = cur;
                                                dirty = true;
                                            }
                                        }
                                    });
                                dirty |= ui
                                    .checkbox(&mut st.force_remux, "강제 변환")
                                    .on_hover_text("컨테이너가 다르면 remux 합니다 (재인코딩 없음)")
                                    .changed();
                            });
                            ui.end_row();
                        } else {
                            ui.label("오디오 포맷");
                            egui::ComboBox::from_id_salt("af")
                                .width(160.0)
                                .selected_text(&st.audio_format)
                                .show_ui(ui, |ui| {
                                    for f in AUDIO_FORMATS {
                                        let mut cur = st.audio_format.clone();
                                        if ui.selectable_value(&mut cur, f.to_string(), f).changed() {
                                            st.audio_format = cur;
                                            dirty = true;
                                        }
                                    }
                                });
                            ui.end_row();

                            ui.label("오디오 화질");
                            egui::ComboBox::from_id_salt("ab")
                                .width(160.0)
                                .selected_text(bitrate_label(&st.audio_bitrate))
                                .show_ui(ui, |ui| {
                                    for b in AUDIO_BITRATES {
                                        let mut cur = st.audio_bitrate.clone();
                                        if ui
                                            .selectable_value(&mut cur, b.to_string(), bitrate_label(b))
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
                    ui.heading("부가 옵션");
                    ui.horizontal_wrapped(|ui| {
                        dirty |= ui.checkbox(&mut st.embed_metadata, "메타데이터 삽입").changed();
                        dirty |= ui.checkbox(&mut st.embed_thumbnail, "썸네일 삽입").changed();
                        dirty |= ui.checkbox(&mut st.embed_chapters, "챕터 삽입").changed();
                        dirty |= ui
                            .checkbox(&mut st.sponsorblock_remove, "SponsorBlock 구간 제거")
                            .changed();
                    });
                    if st.mode == Mode::Video {
                        ui.horizontal_wrapped(|ui| {
                            dirty |= ui.checkbox(&mut st.write_subs, "자막 파일 저장").changed();
                            dirty |= ui.checkbox(&mut st.auto_subs, "자동 생성 자막 포함").changed();
                            dirty |= ui.checkbox(&mut st.embed_subs, "자막 영상에 삽입").changed();
                            ui.label("언어");
                            dirty |= ui
                                .add(egui::TextEdit::singleline(&mut st.sub_langs).desired_width(110.0))
                                .on_hover_text("쉼표로 구분. 예: ko,en")
                                .changed();
                        });
                    }

                    ui.add_space(10.0);
                    ui.heading("플레이리스트");
                    egui::Grid::new("g_pl").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("기본 처리 방식");
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
                                        .selectable_value(&mut st.playlist_mode, m, playlist_mode_label(m))
                                        .changed();
                                }
                            });
                        ui.end_row();

                        ui.label("항목 범위");
                        dirty |= ui
                            .add(
                                egui::TextEdit::singleline(&mut st.playlist_items)
                                    .desired_width(180.0)
                                    .hint_text("예: 1-10, 15, 20-"),
                            )
                            .on_hover_text("비워 두면 전체")
                            .changed();
                        ui.end_row();

                        ui.label("펼치기 최대 개수");
                        dirty |= ui
                            .add(egui::DragValue::new(&mut st.playlist_expand_limit).range(0..=5000))
                            .on_hover_text("0 = 무제한. 채널 전체처럼 큰 목록을 방어합니다.")
                            .changed();
                        ui.end_row();

                        ui.label("");
                        dirty |= ui.checkbox(&mut st.playlist_reverse, "역순으로 받기").changed();
                        ui.end_row();
                    });

                    ui.add_space(10.0);
                    ui.heading("네트워크 / 동시성");
                    egui::Grid::new("g_net").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("동시 다운로드 수");
                        dirty |= ui
                            .add(egui::Slider::new(&mut st.max_concurrent_downloads, 1..=8))
                            .changed();
                        ui.end_row();

                        ui.label("조각 동시 전송");
                        dirty |= ui
                            .add(egui::Slider::new(&mut st.concurrent_fragments, 1..=16))
                            .on_hover_text("yt-dlp -N 옵션")
                            .changed();
                        ui.end_row();

                        ui.label("속도 제한");
                        dirty |= ui
                            .add(
                                egui::TextEdit::singleline(&mut st.rate_limit)
                                    .desired_width(120.0)
                                    .hint_text("예: 2M, 500K"),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("재시도 횟수");
                        dirty |= ui.add(egui::DragValue::new(&mut st.retries).range(0..=100)).changed();
                        ui.end_row();

                        ui.label("프록시");
                        dirty |= ui
                            .add(
                                egui::TextEdit::singleline(&mut st.proxy)
                                    .desired_width(240.0)
                                    .hint_text("http://host:port / socks5://host:port"),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("브라우저 쿠키");
                        egui::ComboBox::from_id_salt("cookies")
                            .width(160.0)
                            .selected_text(if st.cookies_from_browser.is_empty() {
                                "사용 안 함".to_string()
                            } else {
                                st.cookies_from_browser.clone()
                            })
                            .show_ui(ui, |ui| {
                                for b in ["", "chrome", "edge", "firefox", "safari", "brave", "whale", "opera"] {
                                    let label = if b.is_empty() { "사용 안 함" } else { b };
                                    let mut cur = st.cookies_from_browser.clone();
                                    if ui.selectable_value(&mut cur, b.to_string(), label).changed() {
                                        st.cookies_from_browser = cur;
                                        dirty = true;
                                    }
                                }
                            });
                        ui.end_row();
                    });

                    ui.add_space(10.0);
                    ui.heading("동작");
                    ui.vertical(|ui| {
                        dirty |= ui
                            .checkbox(&mut st.paste_to_download, "창에 붙여넣으면 바로 추가 (Ctrl/Cmd+V)")
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.clipboard_watch, "복사하는 즉시 추가 (클립보드 감시)")
                            .on_hover_text("붙여넣지 않아도 URL 을 복사하는 순간 대기열에 들어갑니다")
                            .changed();
                        ui.horizontal_wrapped(|ui| {
                            dirty |= ui.checkbox(&mut st.auto_start, "추가 즉시 다운로드 시작").changed();
                            dirty |= ui.checkbox(&mut st.skip_duplicates, "중복 URL 무시").changed();
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label("테마");
                        for (t, l) in [
                            (ThemePref::System, "시스템"),
                            (ThemePref::Light, "밝게"),
                            (ThemePref::Dark, "어둡게"),
                        ] {
                            if ui.selectable_value(&mut st.theme, t, l).changed() {
                                dirty = true;
                                theme_changed = Some(t);
                            }
                        }
                    });

                    ui.add_space(10.0);
                    ui.heading("도구 (yt-dlp / ffmpeg)");
                    let t = self.tools.lock().unwrap().clone();

                    ui.horizontal_wrapped(|ui| {
                        dirty |= ui
                            .checkbox(&mut st.auto_update_check, "시작할 때 버전 확인")
                            .on_hover_text("하루에 한 번 GitHub 릴리스와 비교합니다")
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.auto_update_tools, "새 버전이면 자동 적용")
                            .on_hover_text("앱이 관리하는 복사본에만 적용됩니다")
                            .changed();
                        dirty |= ui
                            .checkbox(&mut st.auto_install_ffmpeg, "ffmpeg 없으면 자동 설치")
                            .changed();
                    });
                    ui.add_space(4.0);

                    egui::Grid::new("g_bin").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        // ── yt-dlp ──────────────────────
                        ui.label("yt-dlp 실행 파일");
                        ui.horizontal(|ui| {
                            for (m, l) in [
                                (BinMode::Managed, "앱이 관리 (권장)"),
                                (BinMode::System, "시스템 설치본"),
                                (BinMode::Custom, "직접 지정"),
                            ] {
                                if ui.selectable_value(&mut st.bin_mode, m, l).changed() {
                                    dirty = true;
                                    reprobe = true;
                                }
                            }
                        });
                        ui.end_row();

                        if st.bin_mode == BinMode::Custom {
                            ui.label("yt-dlp 경로");
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
                                if ui.small_button("찾기").clicked() {
                                    if let Some(p) = rfd::FileDialog::new().pick_file() {
                                        st.ytdlp_custom_path = p.display().to_string();
                                        dirty = true;
                                        reprobe = true;
                                    }
                                }
                            });
                            ui.end_row();
                        }

                        ui.label("yt-dlp 상태");
                        ui.vertical(|ui| {
                            tool_status_line(ui, &t.ytdlp);
                            ui.horizontal(|ui| {
                                let label = if st.bin_mode == BinMode::Managed {
                                    "지금 설치 / 업데이트"
                                } else {
                                    "yt-dlp -U 실행"
                                };
                                if ui.add_enabled(!t.ytdlp.busy, egui::Button::new(label)).clicked() {
                                    do_install_yt = true;
                                }
                                if ui.add_enabled(!t.ytdlp.busy, egui::Button::new("최신 확인")).clicked() {
                                    do_check = true;
                                }
                            });
                        });
                        ui.end_row();

                        // ── ffmpeg ──────────────────────
                        ui.label("ffmpeg 경로");
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut st.ffmpeg_custom_path)
                                        .desired_width(280.0)
                                        .hint_text("비워 두면 자동 탐색 (앱 관리본 → 시스템 순)"),
                                )
                                .changed()
                            {
                                dirty = true;
                                reprobe = true;
                            }
                            if ui.small_button("찾기").clicked() {
                                if let Some(p) = rfd::FileDialog::new().pick_file() {
                                    st.ffmpeg_custom_path = p.display().to_string();
                                    dirty = true;
                                    reprobe = true;
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("ffmpeg 상태");
                        ui.vertical(|ui| {
                            tool_status_line(ui, &t.ffmpeg);
                            if t.ffmpeg.local.is_none() {
                                if let Some(bad) = tools::broken_ffmpeg(&st) {
                                    ui.label(
                                        RichText::new(format!(
                                            "이 경로의 ffmpeg 이 실행되지 않습니다 (의존 라이브러리 손상 등): {}",
                                            bad.display()
                                        ))
                                        .small()
                                        .color(Color32::from_rgb(220, 110, 110)),
                                    );
                                }
                                ui.label(
                                    RichText::new(
                                        "ffmpeg 이 없으면 고화질 영상·오디오 병합, 오디오 변환, 썸네일 삽입이 제한됩니다.",
                                    )
                                    .small()
                                    .color(Color32::from_rgb(220, 150, 60)),
                                );
                            }
                            if ui
                                .add_enabled(!t.ffmpeg.busy, egui::Button::new("지금 설치 / 업데이트"))
                                .on_hover_text("공식 정적 빌드(약 40MB)를 앱 폴더에 내려받습니다")
                                .clicked()
                            {
                                do_install_ff = true;
                            }
                        });
                        ui.end_row();

                        ui.label("설치 위치");
                        ui.label(
                            RichText::new(crate::tools::managed_dir().display().to_string())
                                .small()
                                .weak(),
                        );
                        ui.end_row();
                    });

                    ui.add_space(10.0);
                    ui.collapsing("고급", |ui| {
                        dirty |= ui
                            .checkbox(&mut st.ignore_yt_dlp_config, "yt-dlp 전역 설정 파일 무시 (--ignore-config)")
                            .on_hover_text("여기 설정이 항상 그대로 적용되도록 합니다")
                            .changed();
                        ui.label("추가 인자");
                        dirty |= ui
                            .add(
                                egui::TextEdit::multiline(&mut st.extra_args)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("예: --geo-bypass --force-ipv4"),
                            )
                            .changed();
                        ui.label(
                            RichText::new(format!("설정 파일: {}", crate::config::config_path().display()))
                                .small()
                                .weak(),
                        );
                    });
                });
            });

        if dirty {
            self.save_settings();
        }
        if let Some(t) = theme_changed {
            apply_theme(ctx, t);
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
            ui.label(RichText::new(format!("설치됨 {v}")).small());
            if let Some(l) = st.latest_pretty() {
                ui.label(RichText::new(format!("· 최신 {l}")).small().weak());
            }
            if !st.managed {
                ui.label(RichText::new("· 시스템 설치본").small().weak());
            }
        }
        None => {
            ui.label(
                RichText::new("설치되지 않음")
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
    match m {
        Mode::Video => "영상",
        Mode::Audio => "오디오",
    }
}

fn bitrate_label(v: &str) -> String {
    if v == "best" {
        "최고 음질".to_string()
    } else {
        format!("{}kbps", v.trim_end_matches(['K', 'k']))
    }
}

fn height_label(h: u32) -> String {
    match h {
        0 => "최고 화질".to_string(),
        2160 => "2160p (4K)".to_string(),
        1440 => "1440p (2K)".to_string(),
        v => format!("{v}p"),
    }
}

fn codec_label(c: &str) -> &str {
    match c {
        "h264" => "H.264 (호환성)",
        "av1" => "AV1 (고효율)",
        "vp9" => "VP9",
        _ => "자동",
    }
}
