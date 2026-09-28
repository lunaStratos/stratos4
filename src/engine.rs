//! 다운로드 대기열 엔진 — 작업 관리, yt-dlp 프로세스 실행, 진행률 파싱,
//! 플레이리스트 해석(펼치기)을 담당한다.

use std::collections::VecDeque;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate};

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
    /// 해석 작업이 목록을 바로 펼치지 않고 사용자 선택을 기다리는지 ('골라서 받기')
    pub pick_mode: bool,
    /// '골라서 받기' 로 읽어 온 목록. 선택을 기다리는 동안에만 Some
    pub pick: Option<PickList>,
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
            pick_mode: false,
            pick: None,
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

/// 목록 해석으로 얻은 항목 하나
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    /// 초 단위 길이 (사이트가 flat 목록에 알려 줄 때만)
    pub duration: Option<i64>,
    /// 업로드 날짜 (사이트가 flat 목록에 알려 줄 때만)
    pub date: Option<NaiveDate>,
    /// YouTube 처럼 "3주 전" 을 날짜로 바꾼 대략값인지
    pub date_approx: bool,
}

/// 사용자가 받을 항목을 고르는 중인 목록
pub struct PickList {
    pub list_title: String,
    pub entries: Vec<Entry>,
    pub checked: Vec<bool>,
    /// UI: 제목 필터
    pub filter: String,
    /// UI: Shift+클릭 범위 선택의 기준 위치
    pub anchor: Option<usize>,
    /// 아직 선택 창을 한 번도 띄우지 않았는지
    pub unseen: bool,
    /// UI: 날짜로 고르기 입력값 (YYYY-MM-DD)
    pub date_input: String,
    /// UI: true = 그 날짜 이후(당일 포함), false = 그 날짜 이전
    pub date_after: bool,
    /// UI: 날짜로 고른 결과 안내
    pub date_msg: String,
}

impl PickList {
    pub fn new(list_title: String, entries: Vec<Entry>) -> Self {
        let month_ago = Local::now()
            .date_naive()
            .checked_sub_months(chrono::Months::new(1))
            .unwrap_or_default();
        Self {
            list_title,
            checked: vec![true; entries.len()],
            entries,
            filter: String::new(),
            anchor: None,
            unseen: true,
            date_input: month_ago.format("%Y-%m-%d").to_string(),
            date_after: true,
            date_msg: String::new(),
        }
    }

    /// 제목 필터에 걸리는 항목 번호들
    pub fn visible(&self) -> Vec<usize> {
        let needle = self.filter.trim().to_lowercase();
        (0..self.entries.len())
            .filter(|&i| {
                needle.is_empty() || self.entries[i].title.to_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn has_dates(&self) -> bool {
        self.entries.iter().any(|e| e.date.is_some())
    }

    /// `among` 가운데 업로드 날짜가 조건(`after` 면 그날 이후, 아니면 그 전)에 맞는
    /// 항목을 `check` 값으로 바꾼다. 날짜를 모르는 항목은 건드리지 않는다.
    /// 반환값: (조건에 맞은 수, 날짜를 몰라 건너뛴 수)
    pub fn apply_date(
        &mut self,
        among: &[usize],
        date: NaiveDate,
        after: bool,
        check: bool,
    ) -> (usize, usize) {
        let (mut hit, mut unknown) = (0, 0);
        for &i in among {
            match self.entries[i].date {
                Some(d) if (d >= date) == after => {
                    self.checked[i] = check;
                    hit += 1;
                }
                Some(_) => {}
                None => unknown += 1,
            }
        }
        (hit, unknown)
    }
}

/// 날짜 입력을 읽는다. 2025-03-14 · 2025.03.14 · 2025/03/14 · 20250314
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    ["%Y-%m-%d", "%Y.%m.%d", "%Y/%m/%d", "%Y%m%d"]
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(s, f).ok())
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

        let mut job = if is_playlist && matches!(pl_mode, PlaylistMode::Expand | PlaylistMode::Pick)
        {
            let mut j = Job::new(self.new_id(), url.to_string(), JobKind::Resolve);
            j.title = t(Key::JobResolving).into();
            j.stage = t(Key::StageReadingList).into();
            j.pick_mode = pl_mode == PlaylistMode::Pick;
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

    /// '골라서 받기' 목록에서 체크된 항목을 대기열에 넣는다. 반환값: 추가된 작업 수
    pub fn add_picked(&self, id: u64) -> usize {
        let st = self.settings.lock().unwrap().clone();
        let mut g = self.jobs.lock().unwrap();
        let Some(pick) = g
            .iter_mut()
            .find(|j| j.id == id)
            .and_then(|j| j.pick.take())
        else {
            return 0;
        };
        let total = pick.entries.len();
        let chosen: Vec<(usize, Entry)> = pick
            .entries
            .into_iter()
            .zip(pick.checked)
            .enumerate()
            .filter(|(_, (_, c))| *c)
            .map(|(i, (e, _))| (i, e))
            .collect();
        let added = insert_entries(
            &mut g,
            id,
            &pick.list_title,
            chosen,
            total,
            &st,
            &self.next_id,
        );
        if let Some(p) = g.iter_mut().find(|x| x.id == id) {
            p.stage = tf(Key::StageAddedItems, &[&added.to_string()]);
        }
        drop(g);
        self.ctx.request_repaint();
        added
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
                Status::Paused if code == ABORT_CANCEL => {
                    j.status = Status::Canceled;
                    j.stage.clear();
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

    /// 실행 중·대기 중·일시정지된 작업을 모두 취소한다.
    pub fn cancel_all(&self) {
        let ids: Vec<u64> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| j.status.is_active() || j.status == Status::Paused)
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.cancel(id);
        }
    }

    pub fn clear_finished(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        // 아직 항목을 고르지 않은 목록은 남겨 둔다.
        jobs.retain(|j| !j.status.is_finished() || j.pick.is_some());
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
                        pick: j.pick_mode,
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
    pick: bool,
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
    let js = tools::js_args();
    let have_js = !js.is_empty();
    args.extend(js);
    args.extend(st.playlist_args(playlist_single));
    args.push(url.clone());

    with_job(&jobs, id, |j| {
        j.stage = t(Key::StageStarting).into();
        j.speed = 0.0;
        if !have_ffmpeg {
            j.push_log(t(Key::LogNoFfmpeg));
        }
        if !have_js {
            j.push_log(t(Key::MsgJsMissingWarn));
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
            add_login_hint(j, &st);
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
        pick,
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
            // YouTube 목록은 "3주 전" 만 알려 주므로 이를 대략적인 날짜로 바꿔 받는다.
            "--extractor-args",
            "youtubetab:approximate_date",
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
    args.extend(st.auth_args());
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
            add_login_hint(j, &st);
        });
        ctx.request_repaint();
        return;
    };

    let list_title = root
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or(t(Key::TitlePlaylist))
        .to_string();

    let mut entries: Vec<Entry> = Vec::new();
    collect_entries(&root, &mut entries);

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

    let mut g = jobs.lock().unwrap();

    // 골라서 받기: 목록만 보관하고 사용자가 선택 창에서 고를 때까지 기다린다.
    // 사용자가 직접 고르므로 펼치기 상한은 적용하지 않는다.
    if pick {
        let n = entries.len();
        if let Some(p) = g.iter_mut().find(|x| x.id == id) {
            p.status = Status::Done;
            p.progress = 1.0;
            p.title = list_title.clone();
            p.stage = tf(Key::StagePickWaiting, &[&n.to_string()]);
            p.finished_at = Some(Local::now());
            p.pick = Some(PickList::new(list_title, entries));
        }
        drop(g);
        ctx.request_repaint();
        return;
    }

    let limit = st.playlist_expand_limit as usize;
    let truncated = limit > 0 && entries.len() > limit;
    if truncated {
        entries.truncate(limit);
    }
    let total = entries.len();
    let added = insert_entries(
        &mut g,
        id,
        &list_title,
        entries.into_iter().enumerate().collect(),
        total,
        &st,
        &next_id,
    );
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
    drop(g);
    ctx.request_repaint();
}

/// 오류가 로그인 부족 때문으로 보이면 설정 방법을 덧붙인다.
/// YouTube 의 JS challenge 를 못 풀어 형식이 빠진 경우도 여기서 안내한다.
fn add_login_hint(j: &mut Job, st: &Settings) {
    let Some(err) = j.error.as_ref() else { return };
    if j.log.iter().any(|l| l.contains("challenge solving failed")) {
        let hint = if tools::resolve_js().is_some() {
            t(Key::ErrJsChallengeFailed)
        } else {
            t(Key::ErrJsRuntimeMissing)
        };
        j.error = Some(format!("{err}\n→ {hint}"));
        return;
    }
    if !needs_login(err) && !j.log.iter().any(|l| needs_login(l)) {
        return;
    }
    let hint = if st.has_auth() {
        t(Key::ErrLoginCookiesRejected)
    } else {
        t(Key::ErrLoginRequired)
    };
    j.error = Some(format!("{err}\n→ {hint}"));
}

/// yt-dlp 가 로그인이 필요할 때 내는 대표적인 오류 문구
pub(crate) fn needs_login(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.starts_with("error")
        && [
            "sign in",
            "login required",
            "log in",
            "private video",
            "private playlist",
            "playlist does not exist",
            "members-only",
            "join this channel",
            "use --cookies",
            "--cookies-from-browser",
            "account cookies",
        ]
        .iter()
        .any(|m| l.contains(m))
}

/// 목록 항목들을 부모(해석) 작업 바로 뒤에 개별 다운로드 작업으로 끼워 넣는다.
/// `entries` 의 번호는 원래 목록에서의 위치(0부터)이고 `total` 은 원래 목록 크기다.
/// 반환값: 실제로 추가된 작업 수 (중복 건너뛰기 반영)
fn insert_entries(
    g: &mut Vec<Job>,
    parent: u64,
    list_title: &str,
    entries: Vec<(usize, Entry)>,
    total: usize,
    st: &Settings,
    next_id: &AtomicU64,
) -> usize {
    let insert_at = g
        .iter()
        .position(|x| x.id == parent)
        .map(|p| p + 1)
        .unwrap_or(g.len());
    let existing: Vec<String> = if st.skip_duplicates {
        g.iter()
            .filter(|j| !matches!(j.status, Status::Failed | Status::Canceled))
            .map(|j| j.url.clone())
            .collect()
    } else {
        Vec::new()
    };

    let mut new_jobs = Vec::with_capacity(entries.len());
    for (idx, e) in entries {
        if st.skip_duplicates && existing.contains(&e.url) {
            continue;
        }
        let mut j = Job::new(
            next_id.fetch_add(1, Ordering::SeqCst),
            e.url,
            JobKind::Download,
        );
        j.title = if e.title.is_empty() {
            format!("{} - {}", list_title, idx + 1)
        } else {
            e.title
        };
        j.parent = Some(parent);
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
    added
}

/// flat-playlist JSON 에서 항목 목록을 재귀적으로 모은다.
/// 채널 URL 처럼 플레이리스트가 중첩된 경우도 처리한다.
fn collect_entries(node: &serde_json::Value, out: &mut Vec<Entry>) {
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
        let duration = e
            .get("duration")
            .and_then(|v| v.as_f64())
            .filter(|d| *d > 0.0)
            .map(|d| d.round() as i64);
        let (date, date_approx) = entry_date(e);
        if let Some(u) = url {
            if u.starts_with("http") {
                out.push(Entry {
                    url: u,
                    title,
                    duration,
                    date,
                    date_approx,
                });
            }
        }
    }
}

/// 항목의 업로드 날짜. `upload_date`(YYYYMMDD) 가 있으면 정확한 값이고,
/// `timestamp` 만 있으면 YouTube 의 "N주 전" 을 바꾼 대략값일 수 있다.
fn entry_date(e: &serde_json::Value) -> (Option<NaiveDate>, bool) {
    if let Some(d) = e
        .get("upload_date")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y%m%d").ok())
    {
        return (Some(d), false);
    }
    let ts = ["timestamp", "release_timestamp"]
        .iter()
        .find_map(|k| e.get(*k).and_then(|v| v.as_i64()));
    let Some(date) = ts
        .and_then(|t| DateTime::from_timestamp(t, 0))
        .map(|d| d.with_timezone(&Local).date_naive())
    else {
        return (None, false);
    };
    let youtube = e
        .get("ie_key")
        .and_then(|v| v.as_str())
        .is_some_and(|k| k.to_ascii_lowercase().contains("youtube"))
        || e.get("url")
            .and_then(|v| v.as_str())
            .is_some_and(|u| u.contains("youtube.com") || u.contains("youtu.be"));
    (Some(date), youtube)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_entries_with_duration() {
        let root = serde_json::json!({
            "title": "channel",
            "entries": [
                { "title": "Videos", "entries": [
                    { "id": "abc", "ie_key": "Youtube", "title": "A", "duration": 61.4,
                      "timestamp": 1_700_000_000 },
                    { "url": "https://example.com/b", "title": "B", "upload_date": "20240131" },
                ]},
                { "url": "not-a-url", "title": "skip" },
            ]
        });
        let mut out = Vec::new();
        collect_entries(&root, &mut out);
        assert_eq!(
            out,
            vec![
                Entry {
                    url: "https://www.youtube.com/watch?v=abc".into(),
                    title: "A".into(),
                    duration: Some(61),
                    date: DateTime::from_timestamp(1_700_000_000, 0)
                        .map(|d| d.with_timezone(&Local).date_naive()),
                    date_approx: true,
                },
                Entry {
                    url: "https://example.com/b".into(),
                    title: "B".into(),
                    duration: None,
                    date: NaiveDate::from_ymd_opt(2024, 1, 31),
                    date_approx: false,
                },
            ]
        );
    }

    #[test]
    fn picks_by_upload_date() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day);
        let mk = |title: &str, date| Entry {
            url: format!("https://example.com/{title}"),
            title: title.into(),
            duration: None,
            date,
            date_approx: false,
        };
        let mut p = PickList::new(
            "list".into(),
            vec![
                mk("old", d(2024, 1, 1)),
                mk("edge", d(2024, 6, 1)),
                mk("new", d(2025, 1, 1)),
                mk("nodate", None),
            ],
        );
        let all = p.visible();
        let cut = d(2024, 6, 1).unwrap();

        // 이후(당일 포함) 빼기 → old 와 날짜 없는 항목만 남는다.
        assert_eq!(p.apply_date(&all, cut, true, false), (2, 1));
        assert_eq!(p.checked, [true, false, false, true]);

        // 이전 빼기 → old 도 빠진다.
        assert_eq!(p.apply_date(&all, cut, false, false), (1, 1));
        assert_eq!(p.checked, [false, false, false, true]);

        // 이후 체크 → edge, new 가 다시 들어간다.
        assert_eq!(p.apply_date(&all, cut, true, true), (2, 1));
        assert_eq!(p.checked, [false, true, true, true]);

        // 제목 필터에 걸린 항목에만 적용된다.
        p.filter = "new".into();
        let vis = p.visible();
        assert_eq!(p.apply_date(&vis, cut, true, false), (1, 0));
        assert_eq!(p.checked, [false, true, false, true]);
    }

    #[test]
    fn parses_date_inputs() {
        let want = NaiveDate::from_ymd_opt(2025, 3, 14);
        for s in ["2025-03-14", " 2025.03.14 ", "2025/03/14", "20250314"] {
            assert_eq!(parse_date(s), want, "{s}");
        }
        assert_eq!(parse_date("2025-13-01"), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn detects_login_errors() {
        assert!(needs_login(
            "ERROR: [youtube:tab] PLxx: The playlist does not exist."
        ));
        assert!(needs_login(
            "ERROR: [youtube] abc: Private video. Sign in if you've been granted access"
        ));
        assert!(!needs_login("ERROR: [youtube] abc: Video unavailable"));
        assert!(!needs_login("[info] Sign in to confirm"));
    }

    #[test]
    fn cookies_file_takes_priority() {
        let mut st = Settings::default();
        assert!(st.auth_args().is_empty());
        st.cookies_from_browser = "firefox".into();
        assert_eq!(st.auth_args(), ["--cookies-from-browser", "firefox"]);
        st.cookies_file = " /tmp/c.txt ".into();
        assert_eq!(st.auth_args(), ["--cookies", "/tmp/c.txt"]);
    }
}
