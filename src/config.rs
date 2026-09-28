//! 사용자 설정 — JSON 파일로 영속화하며 yt-dlp 인자 생성을 담당한다.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::i18n::Lang;
use crate::util::shell_split;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// 영상 + 오디오
    Video,
    /// 오디오만 추출
    Audio,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinMode {
    /// 앱이 직접 내려받아 관리하는 yt-dlp (업데이트 자동 반영)
    Managed,
    /// 시스템에 설치된 yt-dlp (brew / pip / winget 등)
    System,
    /// 사용자가 지정한 경로
    Custom,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemePref {
    System,
    Light,
    Dark,
}

/// 플레이리스트 URL 을 붙여넣었을 때의 처리 방식
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaylistMode {
    /// 목록을 펼쳐 항목마다 개별 작업으로 만든다 (항목별 진행률·재시도 가능)
    Expand,
    /// 목록을 읽은 뒤 받을 항목을 사용자가 체크해서 고른다
    Pick,
    /// 하나의 작업에서 yt-dlp 가 목록 전체를 순차 처리한다
    Single,
    /// 목록을 무시하고 해당 영상 하나만 받는다
    VideoOnly,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    // ── 저장 위치/이름 ────────────────────────────────
    pub download_dir: PathBuf,
    /// 켜면 영상/오디오를 저장 폴더 아래 각각의 하위 폴더에 나눠 담는다
    pub separate_media_dirs: bool,
    pub video_subdir: String,
    pub audio_subdir: String,
    pub output_template: String,
    pub restrict_filenames: bool,
    pub use_archive: bool,

    // ── 화질/포맷 ─────────────────────────────────────
    pub mode: Mode,
    /// 0 = 최고 화질 제한 없음
    pub max_height: u32,
    /// 0 = fps 제한 없음
    pub max_fps: u32,
    /// "auto" | "h264" | "av1" | "vp9"
    pub prefer_codec: String,
    /// "auto" | "mp4" | "mkv" | "webm"
    pub container: String,
    /// 컨테이너로 강제 변환 (--remux-video)
    pub force_remux: bool,
    /// "best" | "mp3" | "m4a" | "opus" | "flac" | "wav"
    pub audio_format: String,
    /// "best"(최고 음질) 또는 비트레이트 문자열 ("320K", "192K", "128K" …)
    pub audio_bitrate: String,

    // ── 부가 작업 ─────────────────────────────────────
    pub embed_thumbnail: bool,
    pub embed_metadata: bool,
    pub embed_chapters: bool,
    pub write_subs: bool,
    pub auto_subs: bool,
    pub embed_subs: bool,
    pub sub_langs: String,
    pub sponsorblock_remove: bool,

    // ── 플레이리스트 ──────────────────────────────────
    pub playlist_mode: PlaylistMode,
    /// "" 이면 전체. 예: "1-10", "3,7,12-"
    pub playlist_items: String,
    /// 펼치기 모드에서 한 번에 만들 최대 작업 수 (0 = 무제한)
    pub playlist_expand_limit: u32,
    /// 플레이리스트 항목을 역순으로
    pub playlist_reverse: bool,

    // ── 네트워크/동시성 ───────────────────────────────
    pub max_concurrent_downloads: usize,
    pub concurrent_fragments: u32,
    /// "" 또는 "2M", "500K"
    pub rate_limit: String,
    pub retries: u32,
    pub proxy: String,
    /// "" | "chrome" | "edge" | "firefox" | "safari" | "brave" | "whale"
    pub cookies_from_browser: String,
    /// Netscape 형식 cookies.txt 경로. 지정하면 다른 로그인 방법보다 우선한다
    pub cookies_file: String,
    /// 앱 전용 로그인 창에 쓴 브라우저 ("" = 안 씀). 평소 브라우저 쿠키보다 우선한다
    pub login_browser: String,

    // ── 동작 ──────────────────────────────────────────
    /// 창에 붙여넣기(Ctrl/Cmd+V) 하면 곧바로 대기열에 추가한다
    pub paste_to_download: bool,
    /// (선택) 클립보드를 계속 지켜보다가 복사되는 즉시 추가한다 — 기본 꺼짐
    pub clipboard_watch: bool,
    /// 추가 즉시 다운로드 시작
    pub auto_start: bool,
    /// 이미 대기열에 있는 URL 중복 추가 방지
    pub skip_duplicates: bool,

    // ── 실행 파일 ─────────────────────────────────────
    pub bin_mode: BinMode,
    pub ytdlp_custom_path: String,
    pub ffmpeg_custom_path: String,
    /// 시작할 때 yt-dlp/ffmpeg 최신 버전을 확인한다
    pub auto_update_check: bool,
    /// 확인 결과 새 버전이 있으면 묻지 않고 바로 적용한다 (관리형일 때만)
    pub auto_update_tools: bool,
    /// 동작하는 ffmpeg 이 없으면 시작 시 자동으로 내려받는다
    pub auto_install_ffmpeg: bool,
    /// JS 런타임이 없으면 Deno 를 자동으로 설치한다 (YouTube 에 필요)
    pub auto_install_deno: bool,
    /// 마지막 업데이트 확인 시각 (unix epoch secs)
    pub last_update_check: i64,
    /// 설정 파일 형식 판. 파일에 없으면(옛 설정) 0 으로 읽혀 한 번만 이관한다
    #[serde(default)]
    pub settings_rev: u32,

    // ── 기타 ──────────────────────────────────────────
    pub theme: ThemePref,
    /// 표시 언어. None 이면 OS 의 언어를 따른다.
    pub language: Option<Lang>,
    pub ignore_yt_dlp_config: bool,
    pub extra_args: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            separate_media_dirs: false,
            video_subdir: "Video".into(),
            audio_subdir: "Audio".into(),
            output_template: "%(title).150B [%(id)s].%(ext)s".into(),
            restrict_filenames: false,
            use_archive: false,

            mode: Mode::Video,
            max_height: 1080,
            max_fps: 0,
            prefer_codec: "auto".into(),
            container: "mp4".into(),
            force_remux: false,
            audio_format: "mp3".into(),
            audio_bitrate: "192K".into(),

            embed_thumbnail: true,
            embed_metadata: true,
            embed_chapters: false,
            write_subs: false,
            auto_subs: false,
            embed_subs: false,
            sub_langs: "ko,en".into(),
            sponsorblock_remove: false,

            playlist_mode: PlaylistMode::Pick,
            playlist_items: String::new(),
            playlist_expand_limit: 200,
            playlist_reverse: false,

            max_concurrent_downloads: 3,
            concurrent_fragments: 4,
            rate_limit: String::new(),
            retries: 10,
            proxy: String::new(),
            cookies_from_browser: String::new(),
            cookies_file: String::new(),
            login_browser: String::new(),

            paste_to_download: true,
            clipboard_watch: false,
            auto_start: true,
            skip_duplicates: true,

            bin_mode: BinMode::Managed,
            ytdlp_custom_path: String::new(),
            ffmpeg_custom_path: String::new(),
            auto_update_check: true,
            auto_update_tools: true,
            auto_install_ffmpeg: true,
            auto_install_deno: true,
            last_update_check: 0,
            settings_rev: SETTINGS_REV,

            theme: ThemePref::System,
            language: None,
            ignore_yt_dlp_config: true,
            extra_args: String::new(),
        }
    }
}

pub fn default_download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 앱 전용 데이터 디렉터리 (설정/관리형 yt-dlp 바이너리 보관)
/// 현재 설정 파일 형식 판 ([`Settings::migrate`] 참고)
const SETTINGS_REV: u32 = 1;

pub fn app_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "stratos", "stratos-dl")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".stratos-dl"))
}

pub fn config_path() -> PathBuf {
    app_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let mut st: Self = match std::fs::read_to_string(config_path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        if st.migrate() {
            st.save();
        }
        st
    }

    /// 옛 설정을 현재 판으로 올린다. 바뀐 게 있으면 true
    fn migrate(&mut self) -> bool {
        if self.settings_rev >= SETTINGS_REV {
            return false;
        }
        // 판 1: 기본 목록 처리 방식이 '펼치기' → '골라서 받기' 로 바뀌었다.
        // 예전 기본값을 그대로 쓰던 경우만 새 기본값으로 옮긴다.
        if self.settings_rev < 1 && self.playlist_mode == PlaylistMode::Expand {
            self.playlist_mode = PlaylistMode::Pick;
        }
        self.settings_rev = SETTINGS_REV;
        true
    }

    pub fn save(&self) {
        let p = config_path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }

    /// 실제로 파일이 저장될 폴더. 분리 저장이 켜져 있으면 하위 폴더까지 포함한다.
    pub fn target_dir(&self) -> PathBuf {
        if !self.separate_media_dirs {
            return self.download_dir.clone();
        }
        let sub = match self.mode {
            Mode::Video => self.video_subdir.trim(),
            Mode::Audio => self.audio_subdir.trim(),
        };
        if sub.is_empty() {
            self.download_dir.clone()
        } else {
            self.download_dir.join(sub)
        }
    }

    /// 설정 → yt-dlp `-f` 포맷 셀렉터.
    /// ffmpeg 가 없으면 병합이 불가능하므로 단일 파일 포맷으로 자동 강등한다.
    pub fn format_selector(&self, have_ffmpeg: bool) -> String {
        if self.mode == Mode::Audio {
            return "ba/b".into();
        }

        let mut filt = String::new();
        if self.max_height > 0 {
            filt.push_str(&format!("[height<={}]", self.max_height));
        }
        if self.max_fps > 0 {
            filt.push_str(&format!("[fps<={}]", self.max_fps));
        }

        let codec = match self.prefer_codec.as_str() {
            "h264" => "[vcodec^=avc1]",
            "av1" => "[vcodec^=av01]",
            "vp9" => "[vcodec^=vp9]",
            _ => "",
        };

        if !have_ffmpeg {
            // 사전 병합된(progressive) 포맷만 사용
            return format!("b{filt}/b");
        }

        let mut s = String::new();
        if !codec.is_empty() {
            s.push_str(&format!("bv*{filt}{codec}+ba/"));
        }
        s.push_str(&format!("bv*{filt}+ba/b{filt}/bv*+ba/b"));
        s
    }

    /// 다운로드 1건에 대한 yt-dlp 인자 목록을 만든다.
    pub fn build_args(
        &self,
        have_ffmpeg: bool,
        ffmpeg_dir: Option<&std::path::Path>,
    ) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());

        if self.ignore_yt_dlp_config {
            push(&mut a, "--ignore-config");
        }
        push(&mut a, "--no-colors");
        push(&mut a, "--newline");
        push(&mut a, "--no-simulate");
        push(&mut a, "--no-quiet");
        push(&mut a, "--progress");

        // 진행률/상태를 기계가 읽을 수 있는 형태로 출력시킨다.
        push(&mut a, "--progress-template");
        a.push(
            "download:@P@%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s\
|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s\
|%(info.playlist_index)s|%(info.n_entries)s|%(info.title)s"
                .into(),
        );
        push(&mut a, "--progress-template");
        a.push("postprocess:@PP@%(progress.status)s|%(progress.postprocessor)s".into());
        push(&mut a, "--print");
        a.push("video:@T@%(title)s".into());
        push(&mut a, "--print");
        a.push("after_move:@F@%(filepath)s".into());

        // 저장 경로/파일명
        push(&mut a, "-P");
        a.push(self.target_dir().to_string_lossy().to_string());
        push(&mut a, "-o");
        a.push(self.output_template.clone());
        if self.restrict_filenames {
            push(&mut a, "--restrict-filenames");
        }
        if self.use_archive {
            push(&mut a, "--download-archive");
            a.push(app_dir().join("archive.txt").to_string_lossy().to_string());
        }

        // 포맷
        push(&mut a, "-f");
        a.push(self.format_selector(have_ffmpeg));

        match self.mode {
            Mode::Audio => {
                push(&mut a, "-x");
                if self.audio_format != "best" {
                    push(&mut a, "--audio-format");
                    a.push(self.audio_format.clone());
                }
                push(&mut a, "--audio-quality");
                // "best" 는 yt-dlp 의 VBR 최고 품질(0)에 해당한다.
                a.push(if self.audio_bitrate == "best" {
                    "0".to_string()
                } else {
                    self.audio_bitrate.clone()
                });
            }
            Mode::Video => {
                if have_ffmpeg && self.container != "auto" {
                    push(&mut a, "--merge-output-format");
                    a.push(self.container.clone());
                    if self.force_remux {
                        push(&mut a, "--remux-video");
                        a.push(self.container.clone());
                    }
                }
            }
        }

        // 부가 작업 (모두 ffmpeg 필요)
        if have_ffmpeg {
            if self.embed_thumbnail {
                push(&mut a, "--embed-thumbnail");
            }
            if self.embed_metadata {
                push(&mut a, "--embed-metadata");
            }
            if self.embed_chapters {
                push(&mut a, "--embed-chapters");
            }
            if self.sponsorblock_remove {
                push(&mut a, "--sponsorblock-remove");
                a.push("sponsor,selfpromo,interaction".into());
            }
        }

        if self.mode == Mode::Video && (self.write_subs || self.auto_subs || self.embed_subs) {
            if self.write_subs {
                push(&mut a, "--write-subs");
            }
            if self.auto_subs {
                push(&mut a, "--write-auto-subs");
            }
            if self.embed_subs && have_ffmpeg {
                push(&mut a, "--embed-subs");
            }
            push(&mut a, "--sub-langs");
            a.push(if self.sub_langs.trim().is_empty() {
                "ko,en".into()
            } else {
                self.sub_langs.clone()
            });
        }

        // 네트워크
        if self.concurrent_fragments > 1 {
            push(&mut a, "-N");
            a.push(self.concurrent_fragments.to_string());
        }
        if !self.rate_limit.trim().is_empty() {
            push(&mut a, "--limit-rate");
            a.push(self.rate_limit.trim().to_string());
        }
        push(&mut a, "--retries");
        a.push(self.retries.to_string());
        push(&mut a, "--fragment-retries");
        a.push(self.retries.to_string());
        if !self.proxy.trim().is_empty() {
            push(&mut a, "--proxy");
            a.push(self.proxy.trim().to_string());
        }
        a.extend(self.auth_args());

        if let Some(dir) = ffmpeg_dir {
            push(&mut a, "--ffmpeg-location");
            a.push(dir.to_string_lossy().to_string());
        }

        a.extend(shell_split(&self.extra_args));
        a
    }

    /// 로그인(쿠키) 인자 — 비공개·회원 전용 영상과 플레이리스트에 필요하다.
    /// 해석 단계와 다운로드 단계에서 공용.
    ///
    /// 우선순위: cookies.txt → 앱 전용 로그인 → 평소 브라우저 쿠키.
    /// 파일과 브라우저를 함께 주면 yt-dlp 가 브라우저 쿠키를 cookies.txt 에 덮어쓰므로
    /// 하나만 쓴다.
    pub fn auth_args(&self) -> Vec<String> {
        let file = self.cookies_file.trim();
        let browser = self.cookies_from_browser.trim();
        if !file.is_empty() {
            vec!["--cookies".into(), file.into()]
        } else if let Some(arg) = crate::login::active_arg(&self.login_browser) {
            vec!["--cookies-from-browser".into(), arg]
        } else if !browser.is_empty() {
            vec!["--cookies-from-browser".into(), browser.into()]
        } else {
            Vec::new()
        }
    }

    pub fn has_auth(&self) -> bool {
        !self.auth_args().is_empty()
    }

    /// 플레이리스트 관련 인자 (해석 단계와 다운로드 단계에서 공용)
    pub fn playlist_args(&self, is_playlist_job: bool) -> Vec<String> {
        let mut a = Vec::new();
        if is_playlist_job {
            a.push("--yes-playlist".to_string());
            if !self.playlist_items.trim().is_empty() {
                a.push("--playlist-items".into());
                a.push(self.playlist_items.trim().to_string());
            }
            if self.playlist_reverse {
                a.push("--playlist-reverse".into());
            }
        } else {
            a.push("--no-playlist".to_string());
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_move_to_pick_once() {
        // 판 표시가 없는 옛 설정: 예전 기본값 '펼치기' → '골라서 받기'
        let mut st: Settings = serde_json::from_str(r#"{"playlist_mode":"Expand"}"#).unwrap();
        assert_eq!(st.settings_rev, 0);
        assert!(st.migrate());
        assert_eq!(st.playlist_mode, PlaylistMode::Pick);
        assert_eq!(st.settings_rev, SETTINGS_REV);

        // 이관 뒤 사용자가 다시 '펼치기' 를 고르면 그대로 둔다.
        st.playlist_mode = PlaylistMode::Expand;
        assert!(!st.migrate());
        assert_eq!(st.playlist_mode, PlaylistMode::Expand);

        // 옛 설정이라도 사용자가 고른 다른 방식은 건드리지 않는다.
        let mut st: Settings = serde_json::from_str(r#"{"playlist_mode":"Single"}"#).unwrap();
        assert!(st.migrate());
        assert_eq!(st.playlist_mode, PlaylistMode::Single);
    }
}
