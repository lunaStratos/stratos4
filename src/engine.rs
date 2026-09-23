//! 다운로드 대기열 엔진 — 작업 관리, yt-dlp 프로세스 실행, 진행률 파싱,
//! 플레이리스트 해석(펼치기)을 담당한다.

use std::collections::VecDeque;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};

use crate::config::{PlaylistMode, Settings};
use crate::i18n::{t, tf, Key};
use crate::util::new_command;
use crate::{tools, util};

pub const ABORT_NONE: u8 = 0;
pub const ABORT_CANCEL: u8 = 1;
pub const ABORT_PAUSE: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// 대기열에서 순서를 기다리는 중
    Queued,
    /// 실행 중
    Running,
    /// 사용자가 일시정지 (부분 파일 유지 → 재개 가능)
    Paused,
    Done,
    Failed,
    Canceled,
}

impl Status {
    pub fn label(&self) -> &'static str {
        t(match self {
            Status::Queued => Key::StQueued,
            Status::Running => Key::StRunning,
            Status::Paused => Key::StPaused,
            Status::Done => Key::StDone,
            Status::Failed => Key::StFailed,
            Status::Canceled => Key::StCanceled,
        })
    }
    pub fn is_active(&self) -> bool {
        matches!(self, Status::Queued | Status::Running)
    }
    pub fn is_finished(&self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Canceled)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobKind {
    /// 실제 미디어 다운로드
    Download,
    /// 플레이리스트를 개별 항목으로 펼치기 위한 해석 작업
    Resolve,
}

pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    pub url: String,
    pub title: String,
    pub status: Status,
    /// 0.0 ~ 1.0
    pub progress: f32,
    pub stage: String,
    pub downloaded: u64,
    pub total: u64,
    pub speed: f64,
    pub eta: i64,
    /// 플레이리스트 일괄 작업일 때 (현재 항목, 전체 항목)
    pub item: Option<(u32, u32)>,
    pub filepath: Option<PathBuf>,
    pub error: Option<String>,
    pub log: VecDeque<String>,
    pub added: DateTime<Local>,
    pub finished_at: Option<DateTime<Local>>,
    /// 목록 펼치기로 생성된 작업이면 부모(해석) 작업 id
    pub parent: Option<u64>,
    /// 이 작업이 플레이리스트 전체를 하나로 처리하는지
    pub playlist_single: bool,
    /// UI: 로그 펼침 여부
    pub show_log: bool,

    child: Arc<Mutex<Option<Child>>>,
    abort: Arc<AtomicU8>,
}

impl Job {
    fn new(id: u64, url: String, kind: JobKind) -> Self {
        Self {
            id,
            kind,
            title: url.clone(),
            url,
            status: Status::Queued,
            progress: 0.0,
            stage: String::new(),
            downloaded: 0,
            total: 0,
            speed: 0.0,
            eta: 0,
            item: None,
            filepath: None,
            error: None,
            log: VecDeque::new(),
            added: Local::now(),
            finished_at: None,
            parent: None,
            playlist_single: false,
            show_log: false,
            child: Arc::new(Mutex::new(None)),
            abort: Arc::new(AtomicU8::new(ABORT_NONE)),
        }
    }

    fn push_log(&mut self, line: impl Into<String>) {
        if self.log.len() >= 400 {
            self.log.pop_front();
        }
        self.log.push_back(line.into());
    }
}

pub struct Engine {
    pub jobs: Arc<Mutex<Vec<Job>>>,
    pub settings: Arc<Mutex<Settings>>,
    next_id: Arc<AtomicU64>,
    ctx: egui::Context,
}

impl Engine {
    pub fn new(ctx: egui::Context, settings: Arc<Mutex<Settings>>) -> Self {
        let e = Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            settings,
            next_id: Arc::new(AtomicU64::new(1)),
            ctx,
        };
        e.spawn_scheduler();
        e
    }

    fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    // ── 작업 추가 ─────────────────────────────────────

    /// URL 하나를 대기열에 넣는다. 플레이리스트로 보이면 설정에 따라 해석 작업을 만든다.
    /// 반환값: 추가된 작업 수 (중복이면 0)
    pub fn add_url(&self, url: &str, mode_override: Option<PlaylistMode>) -> usize {
        let url = url.trim();
        if url.is_empty() {
            return 0;
        }
        let st = self.settings.lock().unwrap().clone();

        if st.skip_duplicates {
            let jobs = self.jobs.lock().unwrap();
            if jobs
                .iter()
                .any(|j| j.url == url && !matches!(j.status, Status::Failed | Status::Canceled))
            {
                return 0;
            }
        }

        let pl_mode = mode_override.unwrap_or(st.playlist_mode);
        let is_playlist = util::looks_like_playlist(url);

        let mut job = if is_playlist && pl_mode == PlaylistMode::Expand {
            let mut j = Job::new(self.new_id(), url.to_string(), JobKind::Resolve);
            j.title = t(Key::JobResolving).into();
            j.stage = t(Key::StageReadingList).into();
            j
        } else {
            let mut j = Job::new(self.new_id(), url.to_string(), JobKind::Download);
            j.playlist_single = is_playlist && pl_mode == PlaylistMode::Single;
            if j.playlist_single {
                j.title = t(Key::TitlePlaylist).into();
            }
            j
        };

        if !st.auto_start && job.kind == JobKind::Download {
            job.status = Status::Paused;
        }
        self.jobs.lock().unwrap().push(job);
        self.ctx.request_repaint();
        1
    }

    pub fn add_many(&self, urls: &[String], mode_override: Option<PlaylistMode>) -> usize {
        urls.iter().map(|u| self.add_url(u, mode_override)).sum()
    }

    // ── 제어 ─────────────────────────────────────────

    pub fn start(&self, id: u64) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            if !matches!(j.status, Status::Running) {
                j.abort.store(ABORT_NONE, Ordering::SeqCst);
                j.status = Status::Queued;
                j.error = None;
                j.finished_at = None;
            }
        }
        self.ctx.request_repaint();
    }

    pub fn pause(&self, id: u64) {
        self.signal(id, ABORT_PAUSE);
    }

    pub fn cancel(&self, id: u64) {
        self.signal(id, ABORT_CANCEL);
    }

    fn signal(&self, id: u64, code: u8) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            j.abort.store(code, Ordering::SeqCst);
            match j.status {
                Status::Running => {
                    if let Some(c) = j.child.lock().unwrap().as_mut() {
                        let _ = c.kill();
                    }
                }
                Status::Queued => {
                    j.status = if code == ABORT_PAUSE {
                        Status::Paused
                    } else {
                        Status::Canceled
                    };
                    j.finished_at = Some(Local::now());
                }
                _ => {}
            }
        }
        self.ctx.request_repaint();
    }

    pub fn remove(&self, id: u64) {
        self.cancel(id);
        let mut jobs = self.jobs.lock().unwrap();
        jobs.retain(|j| j.id != id);
        self.ctx.request_repaint();
    }

    pub fn start_all(&self) {
        let ids: Vec<u64> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| matches!(j.status, Status::Paused | Status::Failed | Status::Canceled))
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.start(id);
        }
    }

    pub fn pause_all(&self) {
        let ids: Vec<u64> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| j.status.is_active())
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.pause(id);
        }
    }

    pub fn clear_finished(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.retain(|j| !j.status.is_finished());
        self.ctx.request_repaint();
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let jobs = self.jobs.lock().unwrap();
        let mut running = 0;
        let mut queued = 0;
        let mut done = 0;
        let mut failed = 0;
        for j in jobs.iter() {
            match j.status {
                Status::Running => running += 1,
                Status::Queued => queued += 1,
                Status::Done => done += 1,
                Status::Failed => failed += 1,
                _ => {}
            }
        }
        (running, queued, done, failed)
    }

    // ── 스케줄러 ──────────────────────────────────────

    fn spawn_scheduler(&self) {
        let jobs = self.jobs.clone();
        let settings = self.settings.clone();
        let ctx = self.ctx.clone();
        let next_id = self.next_id.clone();

        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));

            let st = settings.lock().unwrap().clone();
            let limit = st.max_concurrent_downloads.max(1);

            let mut to_spawn: Vec<Launch> = Vec::new();
            {
                let mut g = jobs.lock().unwrap();
                // 해석 작업은 가볍고 순서에 영향이 크므로 동시 실행 제한과 별개로 처리한다.
                let mut running = g
                    .iter()
                    .filter(|j| j.status == Status::Running && j.kind == JobKind::Download)
                    .count();
                for j in g.iter_mut() {
                    if j.status != Status::Queued {
                        continue;
                    }
                    if j.kind == JobKind::Download {
                        if running >= limit {
                            continue;
                        }
                        running += 1;
                    }
                    j.status = Status::Running;
                    j.error = None;
                    j.abort.store(ABORT_NONE, Ordering::SeqCst);
                    to_spawn.push(Launch {
                        id: j.id,
                        url: j.url.clone(),
                        kind: j.kind,
                        playlist_single: j.playlist_single,
                        child: j.child.clone(),
                        abort: j.abort.clone(),
                    });
                }
            }

            for launch in to_spawn {
                let jobs = jobs.clone();
                let ctx = ctx.clone();
                let settings = settings.clone();
                let next_id = next_id.clone();
                std::thread::spawn(move || match launch.kind {
                    JobKind::Download => run_download(launch, jobs, settings, ctx),
                    JobKind::Resolve => run_resolve(launch, jobs, settings, ctx, next_id),
                });
            }

            ctx.request_repaint_after(Duration::from_millis(400));
        });
    }
}

// ─────────────────────────────────────────────────────────────
// 워커
// ─────────────────────────────────────────────────────────────

type Jobs = Arc<Mutex<Vec<Job>>>;

/// 스케줄러가 워커 스레드에 넘기는 실행 정보
struct Launch {
    id: u64,
    url: String,
    kind: JobKind,
    playlist_single: bool,
    child: Arc<Mutex<Option<Child>>>,
    abort: Arc<AtomicU8>,
}

/// 자식 프로세스 종료를 기다린다.
/// `wait()` 를 잠금 상태로 호출하면 취소 버튼이 UI 를 멈추게 하므로
/// 짧게 잠그고 푸는 `try_wait` 폴링을 사용한다.
fn wait_child(slot: &Arc<Mutex<Option<Child>>>) -> Option<std::process::ExitStatus> {
    let result = loop {
        {
            let mut g = slot.lock().unwrap();
            match g.as_mut() {
                Some(c) => match c.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) => {}
                    Err(_) => break None,
                },
                None => break None,
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    *slot.lock().unwrap() = None;
    result
}

fn with_job<F: FnOnce(&mut Job)>(jobs: &Jobs, id: u64, f: F) {
    let mut g = jobs.lock().unwrap();
    if let Some(j) = g.iter_mut().find(|j| j.id == id) {
        f(j);
    }
}

/// yt-dlp 실행 파일과 ffmpeg 를 확인하고, 없으면 작업을 실패 처리한다.
fn prepare(jobs: &Jobs, id: u64, st: &Settings) -> Option<(PathBuf, bool, Option<PathBuf>)> {
    let Some(bin) = tools::resolve(st) else {
        with_job(jobs, id, |j| {
            j.status = Status::Failed;
            j.error = Some(t(Key::ErrYtdlpNotFoundHint).into());
            j.finished_at = Some(Local::now());
        });
        return None;
    };
    let ffmpeg = tools::resolve_ffmpeg(st);
    let have = ffmpeg.is_some();
    Some((bin, have, ffmpeg))
}

fn run_download(launch: Launch, jobs: Jobs, settings: Arc<Mutex<Settings>>, ctx: egui::Context) {
    let Launch {
        id,
        url,
        playlist_single,
        child: child_slot,
        abort,
        ..
    } = launch;
    let st = settings.lock().unwrap().clone();
    let Some((bin, have_ffmpeg, ffmpeg)) = prepare(&jobs, id, &st) else {
        ctx.request_repaint();
        return;
    };

    let mut args = st.build_args(have_ffmpeg, ffmpeg.as_deref().and_then(|p| p.parent()));
    args.extend(st.playlist_args(playlist_single));
    args.push(url.clone());

    with_job(&jobs, id, |j| {
        j.stage = t(Key::StageStarting).into();
        j.speed = 0.0;
        if !have_ffmpeg {
            j.push_log(t(Key::LogNoFfmpeg));
        }
        j.push_log(format!("$ {} {}", bin.display(), args.join(" ")));
    });
    ctx.request_repaint();

    let spawned = new_command(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            with_job(&jobs, id, |j| {
                j.status = Status::Failed;
                j.error = Some(tf(Key::ErrSpawnFailed, &[&e.to_string()]));
                j.finished_at = Some(Local::now());
            });
            ctx.request_repaint();
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *child_slot.lock().unwrap() = Some(child);

    // stderr 는 별도 스레드에서 로그로 흘려보낸다.
    let err_handle = stderr.map(|s| {
        let jobs = jobs.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            read_lines(s, |line| {
                let is_err = line.starts_with("ERROR");
                with_job(&jobs, id, |j| {
                    j.push_log(line.clone());
                    if is_err && j.error.is_none() {
                        j.error = Some(line.clone());
                    }
                });
                ctx.request_repaint();
            });
        })
    });

    if let Some(out) = stdout {
        let mut last_paint = std::time::Instant::now();
        read_lines(out, |line| {
            handle_line(&jobs, id, playlist_single, &line);
            if last_paint.elapsed().as_millis() > 120 {
                last_paint = std::time::Instant::now();
                ctx.request_repaint();
            }
        });
    }
    if let Some(h) = err_handle {
        let _ = h.join();
    }

    let status = wait_child(&child_slot);

    let abort_code = abort.load(Ordering::SeqCst);
    with_job(&jobs, id, |j| {
        j.speed = 0.0;
        j.eta = 0;
        j.finished_at = Some(Local::now());
        if abort_code == ABORT_PAUSE {
            j.status = Status::Paused;
            j.stage = t(Key::StagePausedResumable).into();
        } else if abort_code == ABORT_CANCEL {
            j.status = Status::Canceled;
            j.stage.clear();
        } else if status.map(|s| s.success()).unwrap_or(false) {
            j.status = Status::Done;
            j.progress = 1.0;
            j.stage = t(Key::StDone).into();
        } else {
            j.status = Status::Failed;
            j.stage.clear();
            if j.error.is_none() {
                j.error = Some(
                    j.log
                        .iter()
                        .rev()
                        .find(|l| l.contains("ERROR"))
                        .cloned()
                        .unwrap_or_else(|| t(Key::ErrUnknownExit).into()),
                );
            }
        }
    });
    ctx.request_repaint();
}

/// 플레이리스트를 flat 하게 읽어 항목별 개별 작업으로 펼친다.
fn run_resolve(
    launch: Launch,
    jobs: Jobs,
    settings: Arc<Mutex<Settings>>,
    ctx: egui::Context,
    next_id: Arc<AtomicU64>,
) {
    let Launch {
        id,
        url,
        child: child_slot,
        abort,
        ..
    } = launch;
    let st = settings.lock().unwrap().clone();
    let Some((bin, _, _)) = prepare(&jobs, id, &st) else {
        ctx.request_repaint();
        return;
    };

    let mut args: Vec<String> = Vec::new();
    if st.ignore_yt_dlp_config {
        args.push("--ignore-config".into());
    }
    args.extend(
        [
            "--flat-playlist",
            "--dump-single-json",
            "--no-warnings",
            "--no-colors",
            "--yes-playlist",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    if !st.playlist_items.trim().is_empty() {
        args.push("--playlist-items".into());
        args.push(st.playlist_items.trim().into());
    }
    if st.playlist_reverse {
        args.push("--playlist-reverse".into());
    }
    if !st.proxy.trim().is_empty() {
        args.push("--proxy".into());
        args.push(st.proxy.trim().into());
    }
    if !st.cookies_from_browser.trim().is_empty() {
        args.push("--cookies-from-browser".into());
        args.push(st.cookies_from_browser.trim().into());
    }
    args.extend(crate::util::shell_split(&st.extra_args));
    args.push(url.clone());

    let spawned = new_command(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            with_job(&jobs, id, |j| {
                j.status = Status::Failed;
                j.error = Some(tf(Key::ErrSpawnFailed, &[&e.to_string()]));
                j.finished_at = Some(Local::now());
            });
            ctx.request_repaint();
            return;
        }
    };
    let mut stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *child_slot.lock().unwrap() = Some(child);

    let err_handle = stderr.map(|s| {
        let jobs = jobs.clone();
        std::thread::spawn(move || {
            read_lines(s, |line| {
                with_job(&jobs, id, |j| {
                    if line.starts_with("ERROR") && j.error.is_none() {
                        j.error = Some(line.clone());
                    }
                    j.push_log(line);
                });
            });
        })
    });

    let mut buf = String::new();
    if let Some(o) = stdout.as_mut() {
        let mut raw = Vec::new();
        let _ = o.read_to_end(&mut raw);
        buf = String::from_utf8_lossy(&raw).into_owned();
    }
    if let Some(h) = err_handle {
        let _ = h.join();
    }
    let status = wait_child(&child_slot);

    if abort.load(Ordering::SeqCst) != ABORT_NONE {
        with_job(&jobs, id, |j| {
            j.status = Status::Canceled;
            j.finished_at = Some(Local::now());
        });
        ctx.request_repaint();
        return;
    }

    let ok = status.map(|s| s.success()).unwrap_or(false);
    let parsed: Option<serde_json::Value> = serde_json::from_str(buf.trim()).ok();

    let Some(root) = parsed.filter(|_| ok) else {
        with_job(&jobs, id, |j| {
            j.status = Status::Failed;
            j.finished_at = Some(Local::now());
            if j.error.is_none() {
                j.error = Some(t(Key::ErrPlaylistReadFailed).into());
            }
        });
        ctx.request_repaint();
        return;
    };

    let list_title = root
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or(t(Key::TitlePlaylist))
        .to_string();

    let mut entries: Vec<(String, String)> = Vec::new();
    collect_entries(&root, &mut entries);

    let limit = st.playlist_expand_limit as usize;
    let truncated = limit > 0 && entries.len() > limit;
    if truncated {
        entries.truncate(limit);
    }

    if entries.is_empty() {
        // 플레이리스트가 아니었던 경우 → 단일 영상으로 처리
        let mut j = Job::new(
            next_id.fetch_add(1, Ordering::SeqCst),
            url.clone(),
            JobKind::Download,
        );
        j.title = list_title.clone();
        if !st.auto_start {
            j.status = Status::Paused;
        }
        let mut g = jobs.lock().unwrap();
        g.push(j);
        if let Some(p) = g.iter_mut().find(|x| x.id == id) {
            p.status = Status::Done;
            p.progress = 1.0;
            p.title = list_title;
            p.stage = t(Key::StageSingleVideo).into();
            p.finished_at = Some(Local::now());
        }
        drop(g);
        ctx.request_repaint();
        return;
    }

    let total = entries.len();
    {
        let mut g = jobs.lock().unwrap();
        let insert_at = g
            .iter()
            .position(|x| x.id == id)
            .map(|p| p + 1)
            .unwrap_or(g.len());
        let existing: Vec<String> = if st.skip_duplicates {
            g.iter().map(|j| j.url.clone()).collect()
        } else {
            Vec::new()
        };

        let mut new_jobs = Vec::with_capacity(total);
        for (idx, (entry_url, title)) in entries.into_iter().enumerate() {
            if st.skip_duplicates && existing.contains(&entry_url) {
                continue;
            }
            let mut j = Job::new(
                next_id.fetch_add(1, Ordering::SeqCst),
                entry_url,
                JobKind::Download,
            );
            j.title = if title.is_empty() {
                format!("{} - {}", list_title, idx + 1)
            } else {
                title
            };
            j.parent = Some(id);
            j.item = Some((idx as u32 + 1, total as u32));
            if !st.auto_start {
                j.status = Status::Paused;
            }
            new_jobs.push(j);
        }
        let added = new_jobs.len();
        for (k, j) in new_jobs.into_iter().enumerate() {
            let at = (insert_at + k).min(g.len());
            g.insert(at, j);
        }
        if let Some(p) = g.iter_mut().find(|x| x.id == id) {
            p.status = Status::Done;
            p.progress = 1.0;
            p.title = list_title;
            p.stage = if truncated {
                tf(
                    Key::StageAddedTruncated,
                    &[&added.to_string(), &limit.to_string()],
                )
            } else {
                tf(Key::StageAddedItems, &[&added.to_string()])
            };
            p.finished_at = Some(Local::now());
        }
    }
    ctx.request_repaint();
}

/// flat-playlist JSON 에서 (url, title) 목록을 재귀적으로 모은다.
/// 채널 URL 처럼 플레이리스트가 중첩된 경우도 처리한다.
fn collect_entries(node: &serde_json::Value, out: &mut Vec<(String, String)>) {
    let Some(entries) = node.get("entries").and_then(|v| v.as_array()) else {
        return;
    };
    for e in entries {
        if e.get("entries").is_some() {
            collect_entries(e, out);
            continue;
        }
        let title = e
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let url = e
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                e.get("webpage_url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                // youtube 등 id 만 오는 경우 보정
                let id = e.get("id").and_then(|v| v.as_str())?;
                let ie = e
                    .get("ie_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if ie.contains("youtube") {
                    Some(format!("https://www.youtube.com/watch?v={id}"))
                } else {
                    None
                }
            });
        if let Some(u) = url {
            if u.starts_with("http") {
                out.push((u, title));
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────
// 출력 파싱
// ─────────────────────────────────────────────────────────────

/// `\n` 과 `\r` 을 모두 줄바꿈으로 취급하며 UTF-8 이 아닌 바이트도 안전하게 처리한다.
fn read_lines<R: Read, F: FnMut(String)>(mut r: R, mut f: F) {
    let mut buf = [0u8; 8192];
    let mut cur: Vec<u8> = Vec::with_capacity(512);
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for &b in &buf[..n] {
                    if b == b'\n' || b == b'\r' {
                        if !cur.is_empty() {
                            f(String::from_utf8_lossy(&cur).trim_end().to_string());
                            cur.clear();
                        }
                    } else {
                        cur.push(b);
                    }
                }
            }
        }
    }
    if !cur.is_empty() {
        f(String::from_utf8_lossy(&cur).trim_end().to_string());
    }
}

fn num(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s == "NA" || s == "None" {
        return None;
    }
    s.parse::<f64>().ok()
}

fn handle_line(jobs: &Jobs, id: u64, playlist_single: bool, line: &str) {
    if let Some(rest) = line.strip_prefix("@P@") {
        let p: Vec<&str> = rest.splitn(9, '|').collect();
        if p.len() < 6 {
            return;
        }
        let downloaded = num(p[1]).unwrap_or(0.0) as u64;
        let total = num(p[2]).or_else(|| num(p[3])).unwrap_or(0.0) as u64;
        let speed = num(p[4]).unwrap_or(0.0);
        let eta = num(p[5]).unwrap_or(0.0) as i64;
        let pl_idx = p.get(6).and_then(|v| num(v)).map(|v| v as u32);
        let pl_n = p.get(7).and_then(|v| num(v)).map(|v| v as u32);
        let title = p.get(8).map(|s| s.trim().to_string()).unwrap_or_default();

        with_job(jobs, id, |j| {
            j.downloaded = downloaded;
            j.total = total;
            j.speed = speed;
            j.eta = eta;
            let frac = if total > 0 {
                (downloaded as f32 / total as f32).clamp(0.0, 1.0)
            } else {
                j.progress
            };
            if playlist_single {
                if let (Some(i), Some(n)) = (pl_idx, pl_n) {
                    if n > 0 {
                        j.item = Some((i, n));
                        j.progress = ((i.saturating_sub(1)) as f32 + frac) / n as f32;
                    }
                } else {
                    j.progress = frac;
                }
            } else {
                j.progress = frac;
            }
            if !title.is_empty() && title != "NA" {
                j.title = if playlist_single {
                    match j.item {
                        Some((i, n)) => format!("[{i}/{n}] {title}"),
                        None => title,
                    }
                } else {
                    title
                };
            }
            j.stage = match p[0] {
                "finished" => t(Key::StageMergeWaiting).into(),
                "error" => t(Key::StageError).into(),
                _ => t(Key::StageDownloading).into(),
            };
        });
        return;
    }

    if let Some(rest) = line.strip_prefix("@PP@") {
        let mut it = rest.splitn(2, '|');
        let status = it.next().unwrap_or("");
        let pp = it.next().unwrap_or("");
        with_job(jobs, id, |j| {
            j.stage = match (status, pp) {
                ("finished", _) => t(Key::StagePostDone).into(),
                (_, p) if !p.is_empty() && p != "NA" => tf(Key::StagePostNamed, &[p]),
                _ => t(Key::StagePost).into(),
            };
        });
        return;
    }

    if let Some(t) = line.strip_prefix("@T@") {
        let t = t.trim().to_string();
        if !t.is_empty() && t != "NA" {
            with_job(jobs, id, |j| {
                if !playlist_single {
                    j.title = t;
                }
            });
        }
        return;
    }

    if let Some(f) = line.strip_prefix("@F@") {
        let f = f.trim().to_string();
        if !f.is_empty() && f != "NA" {
            with_job(jobs, id, |j| j.filepath = Some(PathBuf::from(f)));
        }
        return;
    }

    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    // 일부 상황에서 템플릿이 적용되지 않는 출력에 대한 보조 파싱
    if let Some(dest) = trimmed.strip_prefix("[download] Destination: ") {
        let dest = dest.to_string();
        with_job(jobs, id, |j| {
            j.filepath = Some(PathBuf::from(&dest));
            j.push_log(format!("[download] Destination: {dest}"));
        });
        return;
    }
    with_job(jobs, id, |j| j.push_log(trimmed.to_string()));
}
