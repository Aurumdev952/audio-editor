use crate::audio::{self, Peaks, Player, SAMPLE_RATE};
use crate::export::{self, Progress, ProgressHandle, VideoOptions};
use crate::model::{self, fmt_clock, Caption, EditState, ProjectFile, SubLang};
use crate::translate;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const SPEAKER_COLORS: [Color32; 6] = [
    Color32::from_rgb(90, 150, 230),
    Color32::from_rgb(230, 150, 70),
    Color32::from_rgb(110, 200, 120),
    Color32::from_rgb(200, 110, 200),
    Color32::from_rgb(220, 90, 90),
    Color32::from_rgb(200, 200, 90),
];
const ENGLISH: Color32 = Color32::from_rgb(255, 235, 160);
const PLAYHEAD: Color32 = Color32::from_rgb(255, 220, 80);
const EDGE_GRAB_PX: f32 = 7.0;
const SNAP_PX: f32 = 8.0;
const HEADER_W: f32 = 86.0;
const SPEEDS: [f32; 7] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

/// What the user has selected; Delete and the toolbar act on it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Sel {
    None,
    Clip(usize),
    Caption(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Grab {
    Scrub,
    ClipMove,
    ClipStart,
    ClipEnd,
    CapMove,
    CapStart,
    CapEnd,
}

struct Drag {
    grab: Grab,
    idx: usize,
    press_t: f64,
    /// Clip: (at, end). Caption: (src start, src end).
    orig: (f64, f64),
    /// Snapped ghost start while moving a clip.
    live_at: f64,
    snap: Option<f64>,
}

/// What a right-click landed on.
#[derive(Clone, Copy)]
enum Target {
    Clip(usize, f64),
    Gap(f64),
    Caption(usize, f64),
    Empty(f64),
}

struct Loaded {
    audio_path: String,
    samples: Arc<Vec<f32>>,
    peaks: Peaks,
    src_dur: f64,
    player: Option<Player>,
}

pub struct App {
    loaded: Option<Loaded>,
    project_path: Option<PathBuf>,
    state: EditState,
    undo: Vec<EditState>,
    redo: Vec<EditState>,
    dirty: bool,
    last_autosave: Instant,

    view_start: f64,
    view_len: f64,
    amp: f32,
    follow: bool,
    loop_on: bool,
    sel: Sel,
    drag: Option<Drag>,
    target: Option<Target>,
    last_active: Option<usize>,
    scroll_list_to: Option<usize>,
    focus_text: Option<usize>,
    level: f32,
    hint: &'static str,
    hint_prev: &'static str,

    filter: String,
    only_empty: bool,
    only_untranslated: bool,
    show_sidebar: bool,
    preview_lang: SubLang,
    find: String,
    replace: String,
    show_find: bool,
    show_speakers: bool,
    speaker_edits: Vec<(String, String)>,
    show_video: bool,
    show_shortcuts: bool,
    video_opts: VideoOptions,
    show_translate: bool,
    translate_model: String,
    translate_results: Arc<Mutex<Vec<(f64, f64, Result<String, String>)>>>,
    translate_pending: Arc<Mutex<usize>>,
    progress: Option<ProgressHandle>,
    status: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, args: &[String]) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let mut app = App {
            loaded: None,
            project_path: None,
            state: EditState::default(),
            undo: vec![],
            redo: vec![],
            dirty: false,
            last_autosave: Instant::now(),
            view_start: 0.0,
            view_len: 30.0,
            amp: 1.0,
            follow: true,
            loop_on: false,
            sel: Sel::None,
            drag: None,
            target: None,
            last_active: None,
            scroll_list_to: None,
            focus_text: None,
            level: 0.0,
            hint: "",
            hint_prev: "",
            filter: String::new(),
            only_empty: false,
            only_untranslated: false,
            show_sidebar: true,
            preview_lang: SubLang::Both,
            find: String::new(),
            replace: String::new(),
            show_find: false,
            show_speakers: false,
            speaker_edits: vec![],
            show_video: false,
            show_shortcuts: false,
            video_opts: VideoOptions::default(),
            show_translate: false,
            translate_model: translate::DEFAULT_MODEL.to_string(),
            translate_results: Default::default(),
            translate_pending: Default::default(),
            progress: None,
            status: "Open an audio file or project from the File menu.".into(),
        };
        if let Some(first) = args.first() {
            if first.ends_with(".json") {
                app.open_project(Path::new(first));
            } else {
                app.open_audio(first);
            }
        }
        if let Some(srt) = args.get(1) {
            app.import_srt(Path::new(srt));
        }
        app
    }

    // ---------- loading / saving ----------

    fn open_audio(&mut self, path: &str) {
        match audio::decode(path) {
            Ok(samples) => {
                let peak = samples.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-3);
                let samples = Arc::new(samples);
                let src_dur = samples.len() as f64 / SAMPLE_RATE as f64;
                self.state.ensure_clips(src_dur);
                let player = match Player::new(samples.clone(), self.state.clips.clone()) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        self.status = format!("No audio output: {e}");
                        None
                    }
                };
                self.loaded = Some(Loaded { audio_path: path.to_string(), peaks: Peaks::new(&samples), samples, src_dur, player });
                self.amp = (0.9 / peak).min(20.0);
                self.view_start = 0.0;
                self.view_len = 30.0f64.min(src_dur);
                self.sync_transport();
                self.status = format!("Loaded {path} ({}). Right-click the audio to split it.", fmt_clock(src_dur));
            }
            Err(e) => self.status = format!("Failed to open {path}: {e}"),
        }
    }

    fn open_project(&mut self, path: &Path) {
        let res = std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|s| Ok(serde_json::from_str::<ProjectFile>(&s)?));
        match res {
            Ok(p) => {
                self.state = p.state;
                self.undo.clear();
                self.redo.clear();
                self.sel = Sel::None;
                self.project_path = Some(path.to_path_buf());
                let audio = resolve_relative(path, &p.audio_path);
                self.open_audio(&audio);
                self.dirty = false;
            }
            Err(e) => self.status = format!("Failed to open project: {e}"),
        }
    }

    fn save_project(&mut self, path: Option<PathBuf>) {
        let Some(l) = &self.loaded else { return };
        let path = match path.or_else(|| self.project_path.clone()) {
            Some(p) => p,
            None => match rfd::FileDialog::new().add_filter("Project", &["json"]).set_file_name("project.json").save_file() {
                Some(p) => p,
                None => return,
            },
        };
        let pf = ProjectFile { audio_path: l.audio_path.clone(), state: self.state.clone() };
        match std::fs::write(&path, serde_json::to_string_pretty(&pf).unwrap()) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.project_path = Some(path);
                self.dirty = false;
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    fn autosave(&mut self) {
        if !self.dirty || self.last_autosave.elapsed().as_secs() < 20 {
            return;
        }
        self.last_autosave = Instant::now();
        let Some(l) = &self.loaded else { return };
        let path = match &self.project_path {
            Some(p) => p.with_extension("autosave.json"),
            None => PathBuf::from(format!("{}.autosave.json", l.audio_path)),
        };
        let pf = ProjectFile { audio_path: l.audio_path.clone(), state: self.state.clone() };
        let _ = std::fs::write(path, serde_json::to_string_pretty(&pf).unwrap());
    }

    /// SRT times are timeline times; store them in source time.
    fn srt_to_source(&self, caps: Vec<Caption>) -> Vec<Caption> {
        caps.into_iter()
            .map(|c| Caption { start: self.state.edit_to_src(c.start).unwrap_or(c.start), end: self.state.edit_to_src(c.end - 1e-6).unwrap_or(c.end), ..c })
            .collect()
    }

    fn import_srt(&mut self, path: &Path) {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                self.checkpoint();
                self.state.captions = self.srt_to_source(model::parse_srt(&s));
                self.sel = Sel::None;
                self.status = format!("Imported {} captions from {}", self.state.captions.len(), path.display());
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    fn import_translation_srt(&mut self, path: &Path) {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                self.checkpoint();
                let cues = self.srt_to_source(model::parse_srt(&s));
                let n = model::apply_translation_srt(&mut self.state.captions, &cues);
                self.status = format!("Filled {n} translations from {}", path.display());
            }
            Err(e) => self.status = format!("Import failed: {e}"),
        }
    }

    fn export_srt(&mut self, lang: SubLang) {
        let name = match lang {
            SubLang::Original => "captions.rw.srt",
            SubLang::Translation => "captions.en.srt",
            SubLang::Both => "captions.rw-en.srt",
        };
        let Some(path) = rfd::FileDialog::new().add_filter("SubRip", &["srt"]).set_file_name(name).save_file() else { return };
        let srt = model::write_srt(&self.state.edited_captions(), true, lang);
        self.status = match std::fs::write(&path, srt) {
            Ok(()) => format!("Exported captions to {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    fn start_export(&mut self, video: bool) {
        let Some(l) = &self.loaded else { return };
        let dialog = rfd::FileDialog::new();
        let path = if video {
            dialog.add_filter("Video", &["mp4", "mkv"]).set_file_name("captions_video.mp4").save_file()
        } else {
            dialog.add_filter("Audio", &["m4a", "mp3", "wav", "flac", "ogg"]).set_file_name("edited_audio.m4a").save_file()
        };
        let Some(path) = path else { return };
        let progress: ProgressHandle = Arc::new(Mutex::new(Progress { label: format!("Exporting {}", path.display()), ..Default::default() }));
        self.progress = Some(progress.clone());
        let (samples, state, opts) = (l.samples.clone(), self.state.clone(), self.video_opts.clone());
        std::thread::spawn(move || {
            let res = if video {
                export::export_video(&samples, &state, &path, &opts, &progress)
            } else {
                export::export_audio(&samples, &state, &path, &progress)
            };
            progress.lock().unwrap().done = Some(res.map(|_| format!("Exported {}", path.display())).map_err(|e| e.to_string()));
        });
    }

    // ---------- edit plumbing ----------

    fn checkpoint(&mut self) {
        self.undo.push(self.state.clone());
        if self.undo.len() > 300 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.state, s));
            self.after_change();
            self.status = "Undone".into();
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.state, s));
            self.after_change();
            self.status = "Redone".into();
        }
    }

    fn after_change(&mut self) {
        self.dirty = true;
        self.sel = match self.sel {
            Sel::Clip(i) if i >= self.state.clips.len() => Sel::None,
            Sel::Caption(i) if i >= self.state.captions.len() => Sel::None,
            s => s,
        };
        self.sync_transport();
    }

    /// Span the loop button repeats: the selected clip or caption.
    fn loop_span(&self) -> Option<(f64, f64)> {
        match self.sel {
            Sel::Clip(i) => self.state.clips.get(i).map(|c| (c.at, c.end())),
            Sel::Caption(i) => self.caption_span(i),
            Sel::None => None,
        }
    }

    fn sync_transport(&mut self) {
        let clips = self.state.clips.clone();
        let loop_range = if self.loop_on { self.loop_span() } else { None };
        self.with_transport(|t| {
            t.clips = clips;
            t.loop_range = loop_range;
        });
    }

    fn src_dur(&self) -> f64 {
        self.loaded.as_ref().map_or(0.0, |l| l.src_dur)
    }

    fn dur(&self) -> f64 {
        self.state.duration()
    }

    fn with_transport<R>(&self, f: impl FnOnce(&mut audio::Transport) -> R) -> Option<R> {
        let p = self.loaded.as_ref()?.player.as_ref()?;
        let mut t = p.transport.lock().unwrap();
        Some(f(&mut t))
    }

    fn playhead(&self) -> f64 {
        self.with_transport(|t| t.pos).unwrap_or(0.0)
    }

    fn is_playing(&self) -> bool {
        self.with_transport(|t| t.playing).unwrap_or(false)
    }

    fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.dur());
        self.with_transport(|tr| tr.pos = t);
    }

    fn toggle_play(&mut self) {
        if self.playhead() >= self.dur() - 0.01 {
            self.seek(0.0);
        }
        self.with_transport(|t| {
            t.playing = !t.playing;
            t.stop_at = None;
        });
    }

    fn play_range(&mut self, a: f64, b: f64) {
        self.with_transport(|t| {
            t.pos = a;
            t.stop_at = Some(b);
            t.playing = true;
        });
    }

    /// Timeline span of caption `i` (from its first to its last visible piece).
    fn caption_span(&self, i: usize) -> Option<(f64, f64)> {
        let pieces: Vec<_> = self.state.pieces().into_iter().filter(|p| p.caption == i).collect();
        Some((pieces.first()?.start, pieces.last()?.end))
    }

    // ---------- clip operations ----------

    fn split_at(&mut self, t: f64) {
        let mut probe = self.state.clone();
        if probe.split_at(t).is_none() {
            self.status = "Nothing to split here (too close to a clip edge, or in a gap)".into();
            return;
        }
        self.checkpoint();
        let right = self.state.split_at(t).unwrap();
        self.sel = Sel::Clip(right);
        self.after_change();
        self.status = format!("Split at {}. Click a piece and press Delete to remove it.", fmt_clock(t));
    }

    fn delete_clip(&mut self, i: usize, ripple: bool) {
        if i >= self.state.clips.len() {
            return;
        }
        self.checkpoint();
        let len = self.state.clips[i].len();
        if ripple {
            self.state.ripple_delete(i);
            self.status = format!("Deleted {len:.1}s and closed the gap");
        } else {
            self.state.delete_clip(i);
            self.status = format!("Deleted {len:.1}s, leaving a gap. Drag the next clip left, or right-click the gap → Close gap.");
        }
        self.sel = Sel::None;
        self.after_change();
    }

    fn close_gap(&mut self, t: f64) {
        if self.state.gap_at(t).is_some() {
            self.checkpoint();
            self.state.close_gap(t);
            self.after_change();
            self.status = "Gap closed".into();
        }
    }

    fn close_all_gaps(&mut self) {
        self.checkpoint();
        self.state.close_all_gaps();
        self.after_change();
        self.status = "All gaps closed".into();
    }

    fn delete_selected(&mut self, ripple: bool) {
        match self.sel {
            Sel::Clip(i) => self.delete_clip(i, ripple),
            Sel::Caption(i) => self.delete_caption(i),
            Sel::None => {
                let p = self.playhead();
                if self.state.gap_at(p).is_some() {
                    self.close_gap(p);
                } else {
                    self.status = "Select a clip or caption first (click it)".into();
                }
            }
        }
    }

    // ---------- caption operations ----------

    fn new_caption_at(&mut self, t: f64) {
        let Some(s0) = self.state.edit_to_src(t) else {
            self.status = "Captions belong to audio; put the playhead over a clip".into();
            return;
        };
        let clip = self.state.clips[self.state.clip_at(t).unwrap()];
        let next_cap = self.state.captions.iter().map(|c| c.start).filter(|&s| s > s0).fold(f64::INFINITY, f64::min);
        let s1 = (s0 + 2.0).min(clip.src_end).min(next_cap);
        if s1 - s0 < 0.1 {
            self.status = "No room for a new caption here".into();
            return;
        }
        let speaker = self
            .state
            .captions
            .iter()
            .min_by(|a, b| (a.start - s0).abs().total_cmp(&(b.start - s0).abs()))
            .map(|c| c.speaker.clone())
            .unwrap_or_else(|| "Speaker 1".into());
        self.checkpoint();
        self.state.captions.push(Caption { start: s0, end: s1, speaker, text: String::new(), translation: String::new() });
        self.state.sort_captions();
        if let Some(i) = self.state.captions.iter().position(|c| c.start == s0 && c.end == s1) {
            self.select_caption(i, true);
        }
    }

    fn split_caption_at(&mut self, t: f64) {
        let Some(src) = self.state.edit_to_src(t) else { return };
        let Some(i) = self.state.captions.iter().position(|c| src > c.start + 0.05 && src < c.end - 0.05) else {
            self.status = "No caption under the playhead to split".into();
            return;
        };
        self.checkpoint();
        let c = self.state.captions[i].clone();
        let frac = (src - c.start) / (c.end - c.start);
        let words: Vec<&str> = c.text.split_whitespace().collect();
        let tr: Vec<&str> = c.translation.split_whitespace().collect();
        let (k, kt) = ((words.len() as f64 * frac).round() as usize, (tr.len() as f64 * frac).round() as usize);
        self.state.captions[i] = Caption { end: src, text: words[..k].join(" "), translation: tr[..kt].join(" "), ..c.clone() };
        self.state.captions.insert(i + 1, Caption { start: src, text: words[k..].join(" "), translation: tr[kt..].join(" "), ..c });
        self.sel = Sel::Caption(i + 1);
    }

    fn merge_with_next(&mut self, i: usize) {
        if i + 1 >= self.state.captions.len() {
            return;
        }
        self.checkpoint();
        let next = self.state.captions.remove(i + 1);
        let c = &mut self.state.captions[i];
        c.end = c.end.max(next.end);
        c.text = format!("{} {}", c.text.trim(), next.text.trim()).trim().to_string();
        c.translation = format!("{} {}", c.translation.trim(), next.translation.trim()).trim().to_string();
        self.sel = Sel::Caption(i);
    }

    fn delete_caption(&mut self, i: usize) {
        if i >= self.state.captions.len() {
            return;
        }
        self.checkpoint();
        self.state.captions.remove(i);
        self.sel = Sel::None;
        self.status = "Caption deleted (the audio is untouched)".into();
    }

    fn set_caption_edge(&mut self, start: bool) {
        let Sel::Caption(i) = self.sel else {
            self.status = "Select a caption first".into();
            return;
        };
        let Some(src) = self.state.edit_to_src(self.playhead()) else { return };
        self.checkpoint();
        let c = &mut self.state.captions[i];
        if start {
            c.start = src.min(c.end - 0.05);
        } else {
            c.end = src.max(c.start + 0.05);
        }
        self.state.sort_captions();
    }

    fn select_caption(&mut self, i: usize, focus: bool) {
        self.sel = Sel::Caption(i);
        self.scroll_list_to = Some(i);
        self.show_sidebar = true;
        if focus {
            self.focus_text = Some(i);
        }
    }

    fn replace_all(&mut self) {
        let n = self.count_matches();
        if n > 0 {
            self.checkpoint();
            for c in &mut self.state.captions {
                c.text = c.text.replace(self.find.as_str(), &self.replace);
                c.translation = c.translation.replace(self.find.as_str(), &self.replace);
            }
        }
        self.status = format!("Replaced {n} occurrence(s)");
    }

    fn count_matches(&self) -> usize {
        if self.find.is_empty() {
            return 0;
        }
        self.state.captions.iter().map(|c| c.text.matches(self.find.as_str()).count() + c.translation.matches(self.find.as_str()).count()).sum()
    }

    // ---------- AI translation ----------

    fn translate_job(&self, i: usize) -> translate::Job {
        let caps = &self.state.captions;
        let ctx = |r: std::ops::Range<usize>| caps[r].iter().filter(|c| !c.text.trim().is_empty()).map(|c| c.text.clone()).collect();
        translate::Job {
            start: caps[i].start,
            end: caps[i].end,
            text: caps[i].text.clone(),
            before: ctx(i.saturating_sub(4)..i),
            after: ctx(i + 1..(i + 3).min(caps.len())),
        }
    }

    /// Translate captions `idx` in the background, one request at a time.
    fn start_translation(&mut self, idx: Vec<usize>) {
        if let Err(e) = translate::check_model(&self.translate_model) {
            self.status = e.to_string();
            return;
        }
        let jobs: Vec<translate::Job> = idx.into_iter().filter(|&i| !self.state.captions[i].text.trim().is_empty()).map(|i| self.translate_job(i)).collect();
        if jobs.is_empty() {
            self.status = "Nothing to translate (captions without Kinyarwanda text are skipped)".into();
            return;
        }
        self.status = format!("Translating {} caption(s)…", jobs.len());
        *self.translate_pending.lock().unwrap() += jobs.len();
        let (model, results, pending) = (self.translate_model.clone(), self.translate_results.clone(), self.translate_pending.clone());
        std::thread::spawn(move || {
            for job in jobs {
                let res = translate::translate(&model, &job).map_err(|e| e.to_string());
                results.lock().unwrap().push((job.start, job.end, res));
                *pending.lock().unwrap() -= 1;
            }
        });
    }

    fn collect_translations(&mut self) {
        let done: Vec<_> = std::mem::take(&mut *self.translate_results.lock().unwrap());
        if done.is_empty() {
            return;
        }
        self.checkpoint();
        let (mut ok, mut failed) = (0, 0);
        for (start, end, res) in done {
            match res {
                Ok(t) => {
                    if let Some(c) = self.state.captions.iter_mut().find(|c| c.start == start && c.end == end) {
                        c.translation = t;
                        ok += 1;
                    }
                }
                Err(e) => {
                    failed += 1;
                    self.status = format!("Translation failed: {e}");
                }
            }
        }
        if failed == 0 {
            self.status = format!("Translated {ok} caption(s)");
        }
    }

    // ---------- view helpers ----------

    fn speaker_color(&self, speaker: &str) -> Color32 {
        let i = self.state.speakers().iter().position(|s| s == speaker).unwrap_or(0);
        SPEAKER_COLORS[i % SPEAKER_COLORS.len()]
    }

    fn zoom_around(&mut self, factor: f64, center: f64) {
        let new_len = (self.view_len * factor).clamp(0.2, self.dur().max(1.0) * 1.1);
        let rel = (center - self.view_start) / self.view_len;
        self.view_len = new_len;
        self.view_start = center - rel * new_len;
        self.clamp_view();
    }

    fn zoom_fit(&mut self) {
        self.view_start = 0.0;
        self.view_len = self.dur().max(1.0) * 1.02;
    }

    fn clamp_view(&mut self) {
        let max_len = self.dur().max(1.0) * 1.1;
        self.view_len = self.view_len.clamp(0.2, max_len);
        self.view_start = self.view_start.clamp(0.0, (self.dur() + 2.0 - self.view_len).max(0.0));
    }

    fn t_to_x(&self, t: f64, r: Rect) -> f32 {
        r.left() + ((t - self.view_start) / self.view_len) as f32 * r.width()
    }

    fn x_to_t(&self, x: f32, r: Rect) -> f64 {
        self.view_start + ((x - r.left()) / r.width()) as f64 * self.view_len
    }

    // ---------- keyboard ----------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(Key::F2)) {
            self.show_sidebar = !self.show_sidebar;
        }
        if ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                ctx.memory_mut(|m| m.stop_text_input());
            }
            // Ctrl+Space plays/pauses even while typing a caption
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, Key::Space)) {
                self.toggle_play();
            }
            return;
        }
        let (ctrl, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        if ctrl {
            if pressed(Key::Z) {
                if shift { self.redo() } else { self.undo() }
            } else if pressed(Key::Y) {
                self.redo();
            } else if pressed(Key::S) {
                self.save_project(None);
            } else if pressed(Key::F) {
                self.show_find = true;
            } else if pressed(Key::B) {
                self.split_at(self.playhead());
            }
            return;
        }
        let step = if shift { 5.0 } else { 1.0 };
        if pressed(Key::Space) {
            self.toggle_play();
        }
        if pressed(Key::ArrowLeft) {
            self.seek(self.playhead() - step);
        }
        if pressed(Key::ArrowRight) {
            self.seek(self.playhead() + step);
        }
        if pressed(Key::Comma) {
            self.seek(self.playhead() - 0.05);
        }
        if pressed(Key::Period) {
            self.seek(self.playhead() + 0.05);
        }
        if pressed(Key::Home) {
            self.seek(0.0);
        }
        if pressed(Key::End) {
            self.seek(self.dur());
        }
        if pressed(Key::S) {
            self.split_at(self.playhead());
        }
        if pressed(Key::Delete) || pressed(Key::Backspace) {
            self.delete_selected(shift);
        }
        if pressed(Key::C) {
            self.split_caption_at(self.playhead());
        }
        if pressed(Key::N) {
            self.new_caption_at(self.playhead());
        }
        if pressed(Key::M) {
            if let Sel::Caption(i) = self.sel {
                self.merge_with_next(i);
            }
        }
        if pressed(Key::OpenBracket) {
            self.set_caption_edge(true);
        }
        if pressed(Key::CloseBracket) {
            self.set_caption_edge(false);
        }
        if pressed(Key::L) {
            self.loop_on = !self.loop_on;
            self.sync_transport();
        }
        if pressed(Key::Enter) {
            if let Sel::Caption(i) = self.sel {
                self.select_caption(i, true);
            }
        }
        if pressed(Key::Equals) || pressed(Key::Plus) {
            let p = self.playhead();
            self.zoom_around(0.5, p);
        }
        if pressed(Key::Minus) {
            let p = self.playhead();
            self.zoom_around(2.0, p);
        }
        if pressed(Key::Escape) {
            self.sel = Sel::None;
            self.sync_transport();
        }
    }

    // ---------- top bars ----------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open audio…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Audio", &["m4a", "mp3", "wav", "flac", "ogg", "opus", "aac", "mp4"]).pick_file() {
                        self.state = EditState::default();
                        self.open_audio(&p.to_string_lossy());
                    }
                }
                if ui.button("Open project…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Project", &["json"]).pick_file() {
                        self.open_project(&p);
                    }
                }
                ui.separator();
                if ui.button("Save project   Ctrl+S").clicked() {
                    self.save_project(None);
                }
                if ui.button("Save project as…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Project", &["json"]).save_file() {
                        self.save_project(Some(p));
                    }
                }
                ui.separator();
                if ui.button("Import captions (SRT)…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("SubRip", &["srt"]).pick_file() {
                        self.import_srt(&p);
                    }
                }
                if ui.button("Import English translation (SRT)…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("SubRip", &["srt"]).pick_file() {
                        self.import_translation_srt(&p);
                    }
                }
                ui.separator();
                if ui.button("Export audio…").clicked() {
                    self.start_export(false);
                }
                if ui.button("Export video (black screen + captions)…").clicked() {
                    self.show_video = true;
                }
                ui.menu_button("Export captions (SRT)", |ui| {
                    for lang in SubLang::ALL {
                        if ui.button(lang.label()).clicked() {
                            self.export_srt(lang);
                        }
                    }
                });
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo   Ctrl+Z")).clicked() {
                    self.undo();
                }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo   Ctrl+Shift+Z")).clicked() {
                    self.redo();
                }
                ui.separator();
                if ui.button("Split audio at playhead   S").clicked() {
                    self.split_at(self.playhead());
                }
                if ui.add_enabled(matches!(self.sel, Sel::Clip(_)), egui::Button::new("Delete selected clip   Del")).clicked() {
                    self.delete_selected(false);
                }
                if ui.add_enabled(matches!(self.sel, Sel::Clip(_)), egui::Button::new("Delete clip and close gap   Shift+Del")).clicked() {
                    self.delete_selected(true);
                }
                if ui.button("Close all gaps").clicked() {
                    self.close_all_gaps();
                }
                ui.separator();
                if ui.button("New caption at playhead   N").clicked() {
                    self.new_caption_at(self.playhead());
                }
                if ui.button("Split caption at playhead   C").clicked() {
                    self.split_caption_at(self.playhead());
                }
                if ui.button("Find & replace…   Ctrl+F").clicked() {
                    self.show_find = true;
                }
                if ui.button("Rename speakers…").clicked() {
                    self.show_speakers = true;
                    self.speaker_edits = self.state.speakers().into_iter().map(|s| (s.clone(), s)).collect();
                }
                if ui.button("Translate with AI…").clicked() {
                    self.show_translate = true;
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.show_sidebar, "Captions sidebar   F2");
                ui.checkbox(&mut self.follow, "Follow playhead while playing");
                ui.add(egui::Slider::new(&mut self.amp, 0.5..=20.0).logarithmic(true).text("waveform height"));
                if ui.button("Zoom to fit").clicked() {
                    self.zoom_fit();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    self.show_shortcuts = true;
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("⟲ Undo")).on_hover_text("Ctrl+Z").clicked() {
                self.undo();
            }
            if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("⟳ Redo")).on_hover_text("Ctrl+Shift+Z").clicked() {
                self.redo();
            }
            ui.separator();
            if ui.button("⏪").on_hover_text("back 5 s (Shift+←)").clicked() {
                self.seek(self.playhead() - 5.0);
            }
            let playing = self.is_playing();
            if ui.add(egui::Button::new(if playing { "⏸ Pause" } else { "▶ Play" }).min_size(Vec2::new(78.0, 0.0))).on_hover_text("Space").clicked() {
                self.toggle_play();
            }
            if ui.button("⏩").on_hover_text("forward 5 s (Shift+→)").clicked() {
                self.seek(self.playhead() + 5.0);
            }
            ui.label(egui::RichText::new(format!("{} / {}", fmt_clock(self.playhead()), fmt_clock(self.dur()))).monospace().strong());
            ui.separator();
            if ui.button("✂ Split").on_hover_text("Split the audio at the playhead (S or Ctrl+B)").clicked() {
                self.split_at(self.playhead());
            }
            if ui.add_enabled(self.sel != Sel::None, egui::Button::new("🗑 Delete")).on_hover_text("Delete the selected clip (leaves a gap) or caption (Del)").clicked() {
                self.delete_selected(false);
            }
            if ui
                .add_enabled(matches!(self.sel, Sel::Clip(_)), egui::Button::new("🗑 Delete + close gap"))
                .on_hover_text("Delete the selected clip and pull everything after it left (Shift+Del)")
                .clicked()
            {
                self.delete_selected(true);
            }
            if ui.button("⇔ Close gaps").on_hover_text("Remove every gap between clips").clicked() {
                self.close_all_gaps();
            }
            ui.separator();
            if ui.button("🔍−").on_hover_text("zoom out (−)").clicked() {
                let p = self.playhead();
                self.zoom_around(2.0, p);
            }
            if ui.button("🔍+").on_hover_text("zoom in (+)").clicked() {
                let p = self.playhead();
                self.zoom_around(0.5, p);
            }
            if ui.button("Fit").on_hover_text("show the whole timeline").clicked() {
                self.zoom_fit();
            }
            if ui.selectable_label(self.loop_on, "🔁 Loop").on_hover_text("repeat the selected clip or caption (L)").clicked() {
                self.loop_on = !self.loop_on;
                self.sync_transport();
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.toggle_value(&mut self.show_sidebar, "☰ Captions").on_hover_text("show / hide the captions sidebar (F2)");
                let mut vol = self.with_transport(|t| t.volume).unwrap_or(1.0);
                if ui.add(egui::Slider::new(&mut vol, 0.0..=3.0).show_value(false)).on_hover_text("volume").changed() {
                    self.with_transport(|t| t.volume = vol);
                }
                ui.label("🔊");
                let speed = self.with_transport(|t| t.speed).unwrap_or(1.0);
                egui::ComboBox::from_id_salt("speed").selected_text(format!("{speed}×")).width(58.0).show_ui(ui, |ui| {
                    for s in SPEEDS {
                        if ui.selectable_label(s == speed, format!("{s}×")).clicked() {
                            self.with_transport(|t| t.speed = s);
                        }
                    }
                });
                let (r, _) = ui.allocate_exact_size(Vec2::new(60.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, Color32::from_gray(40));
                let lvl = self.level.min(1.0);
                let col = if lvl > 0.95 { Color32::RED } else { Color32::from_rgb(90, 200, 110) };
                ui.painter().rect_filled(Rect::from_min_size(r.min, Vec2::new(r.width() * lvl, r.height())), 2.0, col);
            });
        });
    }

    // ---------- timeline ----------

    fn timeline(&mut self, ui: &mut egui::Ui) {
        if self.loaded.is_none() {
            ui.centered_and_justified(|ui| ui.label("File ▸ Open audio… or File ▸ Open project…"));
            return;
        }
        let full_w = ui.available_width();
        let (ruler_h, audio_h, cap_h, mini_h) = (24.0, 150.0, 58.0, 28.0);
        let (full, _) = ui.allocate_exact_size(Vec2::new(full_w, ruler_h + audio_h + cap_h + mini_h + 10.0), Sense::hover());
        let header = Rect::from_min_size(full.min, Vec2::new(HEADER_W, full.height()));
        let x0 = full.left() + HEADER_W;
        let w = full.right() - x0;
        let ruler = Rect::from_min_size(Pos2::new(x0, full.top()), Vec2::new(w, ruler_h));
        let audio = Rect::from_min_size(ruler.left_bottom() + Vec2::new(0.0, 2.0), Vec2::new(w, audio_h));
        let caps = Rect::from_min_size(audio.left_bottom() + Vec2::new(0.0, 4.0), Vec2::new(w, cap_h));
        let mini = Rect::from_min_size(caps.left_bottom() + Vec2::new(0.0, 4.0), Vec2::new(w, mini_h));

        let ph = self.playhead();
        if self.follow && self.is_playing() && self.drag.is_none() && (ph < self.view_start || ph > self.view_start + self.view_len * 0.9) {
            self.view_start = ph - self.view_len * 0.1;
        }
        self.clamp_view();

        // wheel: scroll; ctrl+wheel / pinch: zoom around the pointer
        if ui.rect_contains_pointer(ruler.union(audio).union(caps)) {
            let (zoom, scroll, pointer) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
            if zoom != 1.0 {
                let center = pointer.map_or(ph, |p| self.x_to_t(p.x, audio));
                self.zoom_around(1.0 / zoom as f64, center);
            }
            let d = if scroll.x.abs() > scroll.y.abs() { scroll.x } else { scroll.y };
            if d != 0.0 {
                self.view_start -= d as f64 * self.view_len / w as f64;
                self.clamp_view();
            }
        }

        let painter = ui.painter_at(full);
        painter.rect_filled(header, 0.0, Color32::from_gray(24));
        for (r, name) in [(audio, "🔊 Audio"), (caps, "💬 Captions"), (mini, "Overview")] {
            painter.text(Pos2::new(header.left() + 8.0, r.center().y), Align2::LEFT_CENTER, name, FontId::proportional(13.0), Color32::from_gray(190));
        }
        painter.rect_filled(audio, 0.0, Color32::from_gray(16));
        painter.rect_filled(caps, 0.0, Color32::from_gray(20));

        // ruler
        painter.rect_filled(ruler, 0.0, Color32::from_gray(30));
        let tick = nice_tick(self.view_len / (w as f64 / 90.0));
        let mut t = (self.view_start / tick).floor() * tick;
        while t <= self.view_start + self.view_len {
            let x = self.t_to_x(t, audio);
            if x >= x0 {
                painter.line_segment([Pos2::new(x, ruler.bottom() - 6.0), Pos2::new(x, ruler.bottom())], Stroke::new(1.0, Color32::GRAY));
                painter.line_segment([Pos2::new(x, audio.top()), Pos2::new(x, caps.bottom())], Stroke::new(1.0, Color32::from_gray(28)));
                painter.text(Pos2::new(x + 3.0, ruler.top() + 3.0), Align2::LEFT_TOP, fmt_tick(t, tick), FontId::monospace(11.0), Color32::LIGHT_GRAY);
            }
            t += tick;
        }
        let end_x = self.t_to_x(self.dur(), audio);
        if end_x < audio.right() {
            painter.rect_filled(Rect::from_x_y_ranges(end_x.max(x0)..=audio.right(), audio.top()..=caps.bottom()), 0.0, Color32::from_black_alpha(90));
        }

        self.draw_clips(&painter, audio);
        self.draw_caption_pieces(&painter, caps);

        // snap guide while dragging
        if let Some(s) = self.drag.as_ref().and_then(|d| d.snap) {
            let x = self.t_to_x(s, audio);
            painter.line_segment([Pos2::new(x, ruler.top()), Pos2::new(x, caps.bottom())], Stroke::new(1.0, Color32::from_rgb(120, 255, 200)));
        }

        // playhead with a handle on the ruler
        if ph >= self.view_start && ph <= self.view_start + self.view_len {
            let x = self.t_to_x(ph, audio);
            painter.line_segment([Pos2::new(x, ruler.top()), Pos2::new(x, caps.bottom())], Stroke::new(2.0, PLAYHEAD));
            let tri = vec![Pos2::new(x - 6.0, ruler.top()), Pos2::new(x + 6.0, ruler.top()), Pos2::new(x, ruler.top() + 9.0)];
            painter.add(egui::Shape::convex_polygon(tri, PLAYHEAD, Stroke::NONE));
        }

        self.draw_minimap(&painter, mini);

        // --- interactions ---
        let ruler_resp = ui.interact(ruler, egui::Id::unique("ruler"), Sense::click_and_drag());
        if let Some(p) = ruler_resp.interact_pointer_pos() {
            if ruler_resp.clicked() || ruler_resp.dragged() {
                self.seek(self.x_to_t(p.x, audio));
            }
        }
        if ruler_resp.hovered() {
            self.hint = "Click or drag on the ruler to move the playhead";
        }
        self.audio_interaction(ui, audio);
        self.caption_interaction(ui, caps, audio);

        let mini_resp = ui.interact(mini, egui::Id::unique("mini"), Sense::click_and_drag());
        if let Some(p) = mini_resp.interact_pointer_pos() {
            if mini_resp.clicked() || mini_resp.dragged() {
                let t = ((p.x - mini.left()) / mini.width()) as f64 * self.dur().max(1.0);
                self.view_start = t - self.view_len / 2.0;
                self.clamp_view();
            }
        }
        if mini_resp.hovered() {
            self.hint = "Overview: click or drag to scroll the timeline";
        }
    }

    fn draw_clips(&self, painter: &egui::Painter, audio: Rect) {
        let Some(l) = &self.loaded else { return };
        let mid = audio.center().y;
        let half = audio.height() / 2.0 - 10.0;
        let spp = self.view_len / audio.width() as f64;
        let moving = self.drag.as_ref().filter(|d| d.grab == Grab::ClipMove);
        for (i, c) in self.state.clips.iter().enumerate() {
            let (cx0, cx1) = (self.t_to_x(c.at, audio), self.t_to_x(c.end(), audio));
            if cx1 < audio.left() || cx0 > audio.right() {
                continue;
            }
            let block = Rect::from_x_y_ranges(cx0.max(audio.left() - 4.0)..=cx1.min(audio.right() + 4.0), audio.top() + 4.0..=audio.bottom() - 4.0);
            let selected = self.sel == Sel::Clip(i);
            let ghosted = moving.is_some_and(|d| d.idx == i);
            let fill = if selected { Color32::from_rgb(38, 62, 100) } else { Color32::from_rgb(30, 44, 66) };
            painter.rect_filled(block, CornerRadius::same(5), if ghosted { fill.gamma_multiply(0.4) } else { fill });
            // waveform inside the clip
            let wave_painter = painter.with_clip_rect(block.shrink(1.0).intersect(audio));
            let col = if ghosted {
                Color32::from_rgb(70, 100, 140)
            } else if selected {
                Color32::from_rgb(140, 200, 255)
            } else {
                Color32::from_rgb(80, 145, 215)
            };
            let first = (cx0.max(audio.left()) - audio.left()) as usize;
            let last = (cx1.min(audio.right()) - audio.left()) as usize;
            for col_i in first..last {
                let t0 = self.view_start + col_i as f64 * spp;
                let t1 = (t0 + spp).min(c.end());
                if t1 <= c.at {
                    continue;
                }
                let (s0, s1) = (c.to_src(t0.max(c.at)), c.to_src(t1));
                let (lo, hi) = l.peaks.range(&l.samples, (s0 * SAMPLE_RATE as f64) as usize, (s1 * SAMPLE_RATE as f64).ceil() as usize);
                if lo > hi {
                    continue;
                }
                let x = audio.left() + col_i as f32 + 0.5;
                let y0 = mid - (hi * self.amp).clamp(-1.0, 1.0) * half;
                let y1 = mid - (lo * self.amp).clamp(-1.0, 1.0) * half;
                wave_painter.line_segment([Pos2::new(x, y0), Pos2::new(x, y1.max(y0 + 1.0))], Stroke::new(1.0, col));
            }
            let border = if selected { Stroke::new(2.0, Color32::WHITE) } else { Stroke::new(1.0, Color32::from_rgb(70, 100, 150)) };
            painter.rect_stroke(block, CornerRadius::same(5), border, StrokeKind::Inside);
            if selected && block.width() > 30.0 {
                // trim handles
                for x in [block.left() + 3.0, block.right() - 3.0] {
                    painter.line_segment([Pos2::new(x, block.center().y - 14.0), Pos2::new(x, block.center().y + 14.0)], Stroke::new(3.0, Color32::WHITE));
                }
            }
            if block.width() > 70.0 {
                painter.with_clip_rect(block.shrink(4.0).intersect(audio)).text(
                    block.left_top() + Vec2::new(7.0, 5.0),
                    Align2::LEFT_TOP,
                    format!("{} · {:.1}s", fmt_clock(c.src_start), c.len()),
                    FontId::proportional(11.0),
                    Color32::from_gray(170),
                );
            }
        }
        // ghost of the clip being moved
        if let Some(d) = moving {
            let c = self.state.clips[d.idx];
            let r = Rect::from_x_y_ranges(self.t_to_x(d.live_at, audio)..=self.t_to_x(d.live_at + c.len(), audio), audio.top() + 4.0..=audio.bottom() - 4.0);
            painter.rect_filled(r, CornerRadius::same(5), Color32::from_rgba_unmultiplied(120, 180, 255, 70));
            painter.rect_stroke(r, CornerRadius::same(5), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }
    }

    fn draw_caption_pieces(&self, painter: &egui::Painter, caps: Rect) {
        let active = self.state.caption_at_edit(self.playhead());
        for p in self.state.pieces() {
            let (x0, x1) = (self.t_to_x(p.start, caps), self.t_to_x(p.end, caps));
            if x1 < caps.left() || x0 > caps.right() {
                continue;
            }
            let c = &self.state.captions[p.caption];
            let r = Rect::from_x_y_ranges(x0..=x1, caps.top() + 3.0..=caps.bottom() - 3.0);
            let base = self.speaker_color(&c.speaker);
            let fill = if c.text.trim().is_empty() { base.gamma_multiply(0.22) } else { base.gamma_multiply(0.5) };
            painter.rect_filled(r, CornerRadius::same(3), fill);
            let stroke = if self.sel == Sel::Caption(p.caption) {
                Stroke::new(2.0, Color32::WHITE)
            } else if active == Some(p.caption) {
                Stroke::new(1.5, PLAYHEAD)
            } else {
                Stroke::new(1.0, base)
            };
            painter.rect_stroke(r, CornerRadius::same(3), stroke, StrokeKind::Inside);
            let clip = painter.with_clip_rect(r.shrink(3.0).intersect(caps));
            let label = if c.text.trim().is_empty() { "(empty)" } else { c.text.as_str() };
            clip.text(r.left_top() + Vec2::new(4.0, 4.0), Align2::LEFT_TOP, label, FontId::proportional(12.0), Color32::WHITE);
            let sub = if c.translation.trim().is_empty() { c.speaker.as_str() } else { c.translation.as_str() };
            clip.text(r.left_bottom() + Vec2::new(4.0, -4.0), Align2::LEFT_BOTTOM, sub, FontId::proportional(11.0), ENGLISH);
        }
    }

    fn draw_minimap(&self, painter: &egui::Painter, mini: Rect) {
        let Some(l) = &self.loaded else { return };
        painter.rect_filled(mini, 3.0, Color32::from_gray(22));
        let dur = self.dur().max(1.0);
        let cols = mini.width() as usize;
        let spp = dur / cols as f64;
        let h = mini.height() / 2.0 - 2.0;
        for col in 0..cols {
            let t0 = col as f64 * spp;
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for (a, b) in self.state.src_ranges(t0, t0 + spp) {
                let (l0, h0) = l.peaks.range(&l.samples, (a * SAMPLE_RATE as f64) as usize, (b * SAMPLE_RATE as f64) as usize);
                lo = lo.min(l0);
                hi = hi.max(h0);
            }
            if lo <= hi {
                let x = mini.left() + col as f32 + 0.5;
                let y = mini.center().y;
                painter.line_segment([Pos2::new(x, y - (hi * self.amp).min(1.0) * h), Pos2::new(x, y - (lo * self.amp).max(-1.0) * h + 1.0)], Stroke::new(1.0, Color32::from_gray(100)));
            }
        }
        let vx0 = mini.left() + (self.view_start / dur) as f32 * mini.width();
        let vx1 = mini.left() + ((self.view_start + self.view_len) / dur) as f32 * mini.width();
        let view = Rect::from_x_y_ranges(vx0..=vx1.max(vx0 + 3.0).min(mini.right()), mini.y_range());
        painter.rect_filled(view, 3.0, Color32::from_rgba_unmultiplied(120, 190, 255, 30));
        painter.rect_stroke(view, 3.0, Stroke::new(1.5, Color32::from_rgb(120, 190, 255)), StrokeKind::Inside);
        let px = mini.left() + (self.playhead() / dur) as f32 * mini.width();
        painter.line_segment([Pos2::new(px, mini.top()), Pos2::new(px, mini.bottom())], Stroke::new(1.0, PLAYHEAD));
    }

    /// Snap timeline time `t` to clip edges, the playhead or 0 when within a few pixels.
    fn snap(&self, t: f64, exclude: Option<usize>, r: Rect) -> (f64, Option<f64>) {
        let tol = SNAP_PX as f64 * self.view_len / r.width() as f64;
        let mut candidates = vec![0.0, self.playhead()];
        for (i, c) in self.state.clips.iter().enumerate() {
            if Some(i) != exclude {
                candidates.push(c.at);
                candidates.push(c.end());
            }
        }
        candidates
            .into_iter()
            .map(|c| (c, (c - t).abs()))
            .filter(|(_, d)| *d <= tol)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or((t, None), |(c, _)| (c, Some(c)))
    }

    fn clip_hit(&self, x: f32, audio: Rect) -> Option<(usize, Grab)> {
        let t = self.x_to_t(x, audio);
        for (i, c) in self.state.clips.iter().enumerate() {
            let (x0, x1) = (self.t_to_x(c.at, audio), self.t_to_x(c.end(), audio));
            if x < x0 - EDGE_GRAB_PX || x > x1 + EDGE_GRAB_PX {
                continue;
            }
            let wide = x1 - x0 > 3.0 * EDGE_GRAB_PX;
            if wide && (x - x0).abs() <= EDGE_GRAB_PX {
                return Some((i, Grab::ClipStart));
            }
            if wide && (x - x1).abs() <= EDGE_GRAB_PX {
                return Some((i, Grab::ClipEnd));
            }
            if t >= c.at && t < c.end() {
                return Some((i, Grab::ClipMove));
            }
        }
        None
    }

    fn audio_interaction(&mut self, ui: &mut egui::Ui, audio: Rect) {
        let resp = ui.interact(audio, egui::Id::unique("audio"), Sense::click_and_drag());
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let hit = pointer.and_then(|p| self.clip_hit(p.x, audio));

        if resp.hovered() && self.drag.is_none() {
            match hit {
                Some((_, Grab::ClipStart | Grab::ClipEnd)) => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    self.hint = "Drag to trim this end of the clip";
                }
                Some(_) => self.hint = "Click: select + move playhead · Drag: move clip · Right-click: split / delete",
                None => self.hint = "Empty space (silence) · Right-click a gap to close it",
            }
        }

        if resp.drag_started() {
            // measure from where the button went down, not where the drag threshold was crossed
            if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
                let t = self.x_to_t(p.x, audio);
                let hit = self.clip_hit(p.x, audio);
                self.drag = Some(match hit {
                    Some((i, grab)) => {
                        let c = self.state.clips[i];
                        if grab != Grab::ClipMove {
                            self.checkpoint();
                        }
                        self.sel = Sel::Clip(i);
                        Drag { grab, idx: i, press_t: t, orig: (c.at, c.end()), live_at: c.at, snap: None }
                    }
                    None => Drag { grab: Grab::Scrub, idx: 0, press_t: t, orig: (0.0, 0.0), live_at: 0.0, snap: None },
                });
            }
        }
        if resp.dragged() {
            if let (Some(p), Some(d)) = (resp.interact_pointer_pos(), self.drag.as_ref()) {
                let t = self.x_to_t(p.x, audio);
                let (grab, idx, orig, delta) = (d.grab, d.idx, d.orig, t - d.press_t);
                match grab {
                    Grab::Scrub => self.seek(t),
                    Grab::ClipMove => {
                        let len = orig.1 - orig.0;
                        let raw = (orig.0 + delta).max(0.0);
                        // snap either edge of the ghost
                        let (s_start, g1) = self.snap(raw, Some(idx), audio);
                        let (s_end, g2) = self.snap(raw + len, Some(idx), audio);
                        let (live, snap) = if g1.is_some() {
                            (s_start, g1)
                        } else if g2.is_some() {
                            (s_end - len, g2)
                        } else {
                            (raw, None)
                        };
                        let d = self.drag.as_mut().unwrap();
                        d.live_at = live.max(0.0);
                        d.snap = snap;
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    }
                    Grab::ClipStart => {
                        let (t, snap) = self.snap(orig.0 + delta, Some(idx), audio);
                        self.state.trim_start(idx, t);
                        self.drag.as_mut().unwrap().snap = snap;
                        self.sync_transport();
                    }
                    Grab::ClipEnd => {
                        let (t, snap) = self.snap(orig.1 + delta, Some(idx), audio);
                        let sd = self.src_dur();
                        self.state.trim_end(idx, t, sd);
                        self.drag.as_mut().unwrap().snap = snap;
                        self.sync_transport();
                    }
                    _ => {}
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(d) = self.drag.take() {
                if d.grab == Grab::ClipMove && (d.live_at - d.orig.0).abs() > 1e-6 {
                    self.checkpoint();
                    let i = self.state.move_clip(d.idx, d.live_at);
                    self.sel = Sel::Clip(i);
                    self.after_change();
                    self.status = format!("Moved clip to {}", fmt_clock(self.state.clips[i].at));
                }
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.sel = match hit {
                    Some((i, _)) => Sel::Clip(i),
                    None => Sel::None,
                };
                self.seek(self.x_to_t(p.x, audio));
                self.sync_transport();
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = self.x_to_t(p.x, audio);
                self.target = Some(match hit {
                    Some((i, _)) => {
                        self.sel = Sel::Clip(i);
                        Target::Clip(i, t)
                    }
                    None if self.state.gap_at(t).is_some() => Target::Gap(t),
                    None => Target::Empty(t),
                });
            }
        }
        resp.context_menu(|ui| self.context_menu(ui));
    }

    fn caption_interaction(&mut self, ui: &mut egui::Ui, caps: Rect, audio: Rect) {
        let resp = ui.interact(caps, egui::Id::unique("captions"), Sense::click_and_drag());
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let pieces = self.state.pieces();
        let hit = pointer.and_then(|p| {
            pieces.iter().rev().find_map(|pc| {
                let (x0, x1) = (self.t_to_x(pc.start, audio), self.t_to_x(pc.end, audio));
                if p.x < x0 - EDGE_GRAB_PX || p.x > x1 + EDGE_GRAB_PX {
                    return None;
                }
                let wide = x1 - x0 > 3.0 * EDGE_GRAB_PX;
                let grab = if wide && (p.x - x0).abs() <= EDGE_GRAB_PX {
                    Grab::CapStart
                } else if wide && (p.x - x1).abs() <= EDGE_GRAB_PX {
                    Grab::CapEnd
                } else if p.x >= x0 && p.x <= x1 {
                    Grab::CapMove
                } else {
                    return None;
                };
                Some((pc.caption, grab))
            })
        });
        if resp.hovered() && self.drag.is_none() {
            match hit {
                Some((_, Grab::CapStart | Grab::CapEnd)) => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    self.hint = "Drag to change when this caption starts / ends";
                }
                Some(_) => self.hint = "Click: select · Double-click: edit text · Drag: retime · Right-click: more",
                None => self.hint = "Double-click empty space under audio to add a caption",
            }
        }
        if resp.drag_started() {
            if let (Some(p), Some((i, grab))) = (ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()), hit) {
                self.checkpoint();
                let c = &self.state.captions[i];
                self.drag = Some(Drag { grab, idx: i, press_t: self.x_to_t(p.x, audio), orig: (c.start, c.end), live_at: 0.0, snap: None });
                self.sel = Sel::Caption(i);
                self.scroll_list_to = Some(i);
            }
        }
        if resp.dragged() {
            if let (Some(p), Some(d)) = (resp.interact_pointer_pos(), self.drag.as_ref()) {
                let delta = self.x_to_t(p.x, audio) - d.press_t;
                let (o0, o1) = d.orig;
                let (idx, grab) = (d.idx, d.grab);
                let c = &mut self.state.captions[idx];
                match grab {
                    Grab::CapMove => {
                        c.start = (o0 + delta).max(0.0);
                        c.end = c.start + (o1 - o0);
                    }
                    Grab::CapStart => c.start = (o0 + delta).clamp(0.0, o1 - 0.05),
                    Grab::CapEnd => c.end = (o1 + delta).max(o0 + 0.05),
                    _ => {}
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(d) = self.drag.take() {
                if matches!(d.grab, Grab::CapMove | Grab::CapStart | Grab::CapEnd) {
                    let moved = self.state.captions[d.idx].clone();
                    self.state.sort_captions();
                    if let Some(i) = self.state.captions.iter().position(|c| *c == moved) {
                        self.sel = Sel::Caption(i);
                    }
                }
            }
        }
        if resp.clicked() {
            match hit {
                Some((i, _)) => {
                    self.select_caption(i, false);
                    if let Some((a, _)) = self.caption_span(i) {
                        self.seek(a);
                    }
                }
                None => self.sel = Sel::None,
            }
            self.sync_transport();
        }
        if resp.double_clicked() {
            match hit {
                Some((i, _)) => self.select_caption(i, true),
                None => {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.new_caption_at(self.x_to_t(p.x, audio));
                    }
                }
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = self.x_to_t(p.x, audio);
                self.target = Some(match hit {
                    Some((i, _)) => {
                        self.sel = Sel::Caption(i);
                        Target::Caption(i, t)
                    }
                    None => Target::Empty(t),
                });
            }
        }
        resp.context_menu(|ui| self.context_menu(ui));
    }

    fn context_menu(&mut self, ui: &mut egui::Ui) {
        let Some(target) = self.target else {
            ui.close();
            return;
        };
        let mut done = false;
        match target {
            Target::Clip(i, t) => {
                ui.label(egui::RichText::new(format!("Audio clip · {}", fmt_clock(t))).weak());
                if ui.button("✂  Split here").clicked() {
                    self.split_at(t);
                    done = true;
                }
                if ui.button("✂  Split at playhead").clicked() {
                    self.split_at(self.playhead());
                    done = true;
                }
                ui.separator();
                if ui.button("🗑  Delete clip (leave gap)").clicked() {
                    self.delete_clip(i, false);
                    done = true;
                }
                if ui.button("🗑  Delete clip and close gap").clicked() {
                    self.delete_clip(i, true);
                    done = true;
                }
                ui.separator();
                if ui.button("▶  Play this clip").clicked() {
                    let c = self.state.clips[i];
                    self.play_range(c.at, c.end());
                    done = true;
                }
                if ui.button("💬  New caption here").clicked() {
                    self.new_caption_at(t);
                    done = true;
                }
            }
            Target::Gap(t) => {
                let (a, b) = self.state.gap_at(t).unwrap_or((t, t));
                ui.label(egui::RichText::new(format!("Gap · {:.1}s of silence", b - a)).weak());
                if ui.button("⇔  Close this gap").clicked() {
                    self.close_gap(t);
                    done = true;
                }
                if ui.button("⇔  Close all gaps").clicked() {
                    self.close_all_gaps();
                    done = true;
                }
            }
            Target::Caption(i, t) => {
                ui.label(egui::RichText::new("Caption").weak());
                if ui.button("✏  Edit text").clicked() {
                    self.select_caption(i, true);
                    done = true;
                }
                if ui.button("▶  Play caption").clicked() {
                    if let Some((a, b)) = self.caption_span(i) {
                        self.play_range(a, b);
                    }
                    done = true;
                }
                if ui.button("✂  Split caption here").clicked() {
                    self.split_caption_at(t);
                    done = true;
                }
                if ui.button("⤓  Merge with next caption").clicked() {
                    self.merge_with_next(i);
                    done = true;
                }
                if ui.button("🌐  Translate with AI").clicked() {
                    self.start_translation(vec![i]);
                    done = true;
                }
                ui.separator();
                if ui.button("🗑  Delete caption").clicked() {
                    self.delete_caption(i);
                    done = true;
                }
            }
            Target::Empty(t) => {
                if ui.button("Move playhead here").clicked() {
                    self.seek(t);
                    done = true;
                }
                if ui.button("⇔  Close all gaps").clicked() {
                    self.close_all_gaps();
                    done = true;
                }
            }
        }
        if done {
            self.target = None;
            ui.close();
        }
    }

    // ---------- preview ----------

    fn preview(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Preview:");
            for lang in SubLang::ALL {
                ui.selectable_value(&mut self.preview_lang, lang, lang.label());
            }
        });
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ui.available_height().max(80.0)), Sense::hover());
        let painter = ui.painter_at(r);
        painter.rect_filled(r, 4.0, Color32::BLACK);
        let Some(i) = self.state.caption_at_edit(self.playhead()) else { return };
        let c = &self.state.captions[i];
        let mut job = egui::text::LayoutJob::default();
        for (k, line) in self.preview_lang.lines(c).iter().enumerate() {
            let english = self.preview_lang == SubLang::Translation || k == 1;
            let fmt = egui::TextFormat {
                font_id: FontId::proportional(if k == 1 { 21.0 } else { 26.0 }),
                color: if english { ENGLISH } else { Color32::WHITE },
                italics: k == 1,
                ..Default::default()
            };
            job.append(&if k > 0 { format!("\n{line}") } else { line.clone() }, 0.0, fmt);
        }
        job.wrap.max_width = r.width() * 0.85;
        job.halign = Align::Center;
        let galley = painter.layout_job(job);
        let pos = Pos2::new(r.center().x, r.center().y - galley.size().y / 2.0 + 10.0);
        painter.text(Pos2::new(r.center().x, pos.y - 6.0), Align2::CENTER_BOTTOM, &c.speaker, FontId::proportional(15.0), self.speaker_color(&c.speaker));
        painter.galley(pos, galley, Color32::WHITE);
    }

    // ---------- sidebar ----------

    fn caption_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(format!("Captions ({})", self.state.captions.len()));
            if ui.button("+ New at playhead").on_hover_text("N").clicked() {
                self.new_caption_at(self.playhead());
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("🔎 search text…").desired_width(170.0));
            ui.checkbox(&mut self.only_empty, "empty only");
            ui.checkbox(&mut self.only_untranslated, "no English");
        });
        ui.separator();

        let snapshot = self.state.clone();
        let speakers = self.state.speakers();
        let active = self.state.caption_at_edit(self.playhead());
        if self.follow && self.is_playing() && active != self.last_active && active.is_some() && self.focus_text.is_none() {
            self.scroll_list_to = active;
        }
        self.last_active = active;
        let spans: Vec<Option<(f64, f64)>> = (0..self.state.captions.len()).map(|i| self.caption_span(i)).collect();
        let filter = self.filter.to_lowercase();
        let mut gained_focus = false;
        let mut action: Option<ListAction> = None;
        let mut scroll_to = self.scroll_list_to.take();
        let focus_text = self.focus_text.take();
        let sel = self.sel;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for i in 0..self.state.captions.len() {
                let c = &mut self.state.captions[i];
                if self.only_empty && !c.text.trim().is_empty() {
                    continue;
                }
                if self.only_untranslated && (c.text.trim().is_empty() || !c.translation.trim().is_empty()) {
                    continue;
                }
                if !filter.is_empty()
                    && !c.text.to_lowercase().contains(&filter)
                    && !c.translation.to_lowercase().contains(&filter)
                    && !c.speaker.to_lowercase().contains(&filter)
                {
                    continue;
                }
                let span = spans[i];
                let is_sel = sel == Sel::Caption(i);
                let color = SPEAKER_COLORS[speakers.iter().position(|s| *s == c.speaker).unwrap_or(0) % SPEAKER_COLORS.len()];
                let frame = egui::Frame::group(ui.style())
                    .fill(if is_sel {
                        Color32::from_rgb(40, 50, 70)
                    } else if active == Some(i) {
                        Color32::from_rgb(45, 45, 30)
                    } else {
                        Color32::from_gray(28)
                    })
                    .stroke(Stroke::new(if is_sel { 2.0 } else { 1.0 }, color));
                let resp = frame
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.selectable_label(is_sel, format!("#{}", i + 1)).on_hover_text("select and jump here").clicked() {
                                action = Some(ListAction::Select(i));
                            }
                            match span {
                                Some((a, b)) => {
                                    let (mut na, mut nb) = (a, b);
                                    let ra = ui.add(egui::DragValue::new(&mut na).speed(0.01).custom_formatter(|v, _| fmt_clock(v))).on_hover_text("start (drag or type)");
                                    ui.label("→");
                                    let rb = ui.add(egui::DragValue::new(&mut nb).speed(0.01).custom_formatter(|v, _| fmt_clock(v))).on_hover_text("end (drag or type)");
                                    if ra.drag_started() || rb.drag_started() || ra.gained_focus() || rb.gained_focus() {
                                        gained_focus = true;
                                    }
                                    if ra.changed() || rb.changed() {
                                        action = Some(ListAction::Shift(i, na - a, nb - b));
                                    }
                                }
                                None => {
                                    ui.label(egui::RichText::new("audio deleted").weak().italics());
                                }
                            }
                            egui::ComboBox::from_id_salt(("spk", i)).selected_text(&c.speaker).width(90.0).show_ui(ui, |ui| {
                                for s in &speakers {
                                    if ui.selectable_label(*s == c.speaker, s).clicked() {
                                        action = Some(ListAction::Speaker(i, s.clone()));
                                    }
                                }
                            });
                            if ui.small_button("▶").on_hover_text("play this caption").clicked() {
                                action = Some(ListAction::Play(i));
                            }
                            ui.menu_button("⋯", |ui| {
                                if ui.button("⤓ Merge with next").clicked() {
                                    action = Some(ListAction::Merge(i));
                                }
                                if ui.button("🌐 Translate with AI").clicked() {
                                    action = Some(ListAction::Translate(i));
                                }
                                if ui.button("🗑 Delete caption").clicked() {
                                    action = Some(ListAction::Delete(i));
                                }
                            });
                        });
                        let te = ui.add(egui::TextEdit::multiline(&mut c.text).desired_rows(1).desired_width(f32::INFINITY).hint_text("Kinyarwanda (missing — type it)"));
                        if te.gained_focus() {
                            gained_focus = true;
                            action = Some(ListAction::Focus(i));
                        }
                        if focus_text == Some(i) {
                            te.request_focus();
                        }
                        let tr = ui.add(
                            egui::TextEdit::multiline(&mut c.translation).desired_rows(1).desired_width(f32::INFINITY).text_color(ENGLISH).hint_text("English translation"),
                        );
                        if tr.gained_focus() {
                            gained_focus = true;
                            action = Some(ListAction::Focus(i));
                        }
                    })
                    .response;
                if scroll_to == Some(i) {
                    resp.scroll_to_me(Some(Align::Center));
                    scroll_to = None;
                }
            }
        });

        if gained_focus {
            self.undo.push(snapshot);
            self.redo.clear();
        }
        if self.undo.last().is_some_and(|u| *u != self.state) {
            self.dirty = true;
        }
        match action {
            Some(ListAction::Select(i)) => {
                self.sel = Sel::Caption(i);
                if let Some((a, _)) = self.caption_span(i) {
                    self.seek(a);
                }
                self.sync_transport();
            }
            Some(ListAction::Focus(i)) => self.sel = Sel::Caption(i),
            Some(ListAction::Shift(i, da, db)) => {
                let c = &mut self.state.captions[i];
                c.start += da;
                c.end = (c.end + db).max(c.start + 0.05);
                self.dirty = true;
            }
            Some(ListAction::Speaker(i, s)) => {
                self.checkpoint();
                self.state.captions[i].speaker = s;
            }
            Some(ListAction::Play(i)) => {
                if let Some((a, b)) = self.caption_span(i) {
                    self.play_range(a, b);
                }
            }
            Some(ListAction::Merge(i)) => self.merge_with_next(i),
            Some(ListAction::Delete(i)) => self.delete_caption(i),
            Some(ListAction::Translate(i)) => self.start_translation(vec![i]),
            None => {}
        }
    }

    // ---------- windows ----------

    fn windows(&mut self, ctx: &egui::Context) {
        let mut open = self.show_find;
        egui::Window::new("Find & replace").open(&mut open).resizable(false).show(ctx, |ui| {
            egui::Grid::new("fr").show(ui, |ui| {
                ui.label("Find");
                ui.text_edit_singleline(&mut self.find);
                ui.end_row();
                ui.label("Replace");
                ui.text_edit_singleline(&mut self.replace);
                ui.end_row();
            });
            ui.label(format!("{} match(es) in Kinyarwanda + English (case-sensitive)", self.count_matches()));
            ui.horizontal(|ui| {
                if ui.button("Show matches").clicked() {
                    self.filter = self.find.clone();
                    self.show_sidebar = true;
                }
                if ui.button("Replace all").clicked() {
                    self.replace_all();
                }
            });
        });
        self.show_find = open;

        let mut open = self.show_speakers;
        let mut apply = false;
        egui::Window::new("Rename speakers").open(&mut open).resizable(false).show(ctx, |ui| {
            ui.label("Rename a speaker everywhere (e.g. Speaker 1 → Alice):");
            for (i, (old, new)) in self.speaker_edits.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, SPEAKER_COLORS[i % SPEAKER_COLORS.len()]);
                    ui.label(old.as_str());
                    ui.label("→");
                    ui.text_edit_singleline(new);
                });
            }
            ui.horizontal(|ui| {
                if ui.button("Add speaker").clicked() {
                    let name = format!("Speaker {}", self.speaker_edits.len() + 1);
                    self.speaker_edits.push((name.clone(), name));
                }
                apply = ui.button("Apply").clicked();
            });
        });
        if apply {
            self.checkpoint();
            for (old, new) in &self.speaker_edits {
                for c in self.state.captions.iter_mut().filter(|c| c.speaker == *old) {
                    c.speaker = new.clone();
                }
            }
            self.speaker_edits = self.state.speakers().into_iter().map(|s| (s.clone(), s)).collect();
        }
        self.show_speakers = open;

        let mut open = self.show_video;
        let mut go = false;
        egui::Window::new("Export video").open(&mut open).resizable(false).show(ctx, |ui| {
            let o = &mut self.video_opts;
            ui.horizontal(|ui| {
                ui.label("Resolution");
                for (w, h) in [(1280, 720), (1920, 1080), (1080, 1920)] {
                    if ui.selectable_label(o.width == w && o.height == h, format!("{w}×{h}")).clicked() {
                        o.width = w;
                        o.height = h;
                        o.font_size = (h.min(w) as f32 * 0.06) as u32;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Subtitles");
                for lang in SubLang::ALL {
                    ui.radio_value(&mut o.lang, lang, lang.label());
                }
            });
            ui.add(egui::Slider::new(&mut o.font_size, 20..=140).text("font size"));
            ui.checkbox(&mut o.show_speaker, "show speaker name");
            ui.checkbox(&mut o.color_by_speaker, "colour captions by speaker");
            go = ui.button("Export…").clicked();
        });
        self.show_video = open && !go;
        if go {
            self.start_export(true);
        }

        let mut open = self.show_translate;
        let mut run: Option<Vec<usize>> = None;
        egui::Window::new("Translate with AI").open(&mut open).resizable(false).show(ctx, |ui| {
            ui.label("Kinyarwanda → English through OpenRouter (key from OPENROUTER_API_KEY). Claude models are blocked.");
            ui.horizontal(|ui| {
                ui.label("Model");
                ui.text_edit_singleline(&mut self.translate_model);
            });
            if let Err(e) = translate::check_model(&self.translate_model) {
                ui.colored_label(Color32::RED, e.to_string());
            }
            let missing: Vec<usize> = (0..self.state.captions.len())
                .filter(|&i| !self.state.captions[i].text.trim().is_empty() && self.state.captions[i].translation.trim().is_empty())
                .collect();
            ui.horizontal(|ui| {
                if ui.button(format!("Translate {} missing", missing.len())).clicked() {
                    run = Some(missing.clone());
                }
                if let Sel::Caption(i) = self.sel {
                    if ui.button(format!("Re-translate caption #{}", i + 1)).clicked() {
                        run = Some(vec![i]);
                    }
                }
            });
            let pending = *self.translate_pending.lock().unwrap();
            if pending > 0 {
                ui.label(format!("{pending} request(s) in progress…"));
            }
        });
        self.show_translate = open;
        if let Some(idx) = run {
            self.start_translation(idx);
        }
        self.collect_translations();
        if *self.translate_pending.lock().unwrap() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        let mut open = self.show_shortcuts;
        egui::Window::new("Keyboard shortcuts").open(&mut open).resizable(false).show(ctx, |ui| {
            egui::Grid::new("keys").striped(true).show(ui, |ui| {
                for (k, v) in SHORTCUTS {
                    ui.monospace(*k);
                    ui.label(*v);
                    ui.end_row();
                }
            });
        });
        self.show_shortcuts = open;

        if let Some(p) = self.progress.clone() {
            let mut pr = p.lock().unwrap();
            if let Some(done) = pr.done.take() {
                self.status = match done {
                    Ok(s) => s,
                    Err(e) => format!("Export failed: {e}"),
                };
                drop(pr);
                self.progress = None;
            } else {
                egui::Window::new("Exporting").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
                    ui.label(&pr.label);
                    ui.add(egui::ProgressBar::new(pr.fraction).show_percentage().desired_width(360.0));
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }
}

enum ListAction {
    Select(usize),
    Focus(usize),
    Shift(usize, f64, f64),
    Speaker(usize, String),
    Play(usize),
    Merge(usize),
    Delete(usize),
    Translate(usize),
}

const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "play / pause (Ctrl+Space while typing)"),
    ("← / →", "move playhead 1 s (Shift: 5 s)"),
    (", / .", "move playhead 0.05 s"),
    ("Home / End", "start / end"),
    ("S  or  Ctrl+B", "split the audio at the playhead"),
    ("Del", "delete selected clip (leaves a gap) or caption"),
    ("Shift+Del", "delete selected clip and close the gap"),
    ("Right-click", "split / delete / close gap / caption options"),
    ("Drag a clip", "move it (snaps to edges and playhead)"),
    ("Drag a clip edge", "trim it"),
    ("N", "new caption at playhead"),
    ("C", "split the caption at the playhead"),
    ("M", "merge selected caption with the next"),
    ("[ / ]", "set selected caption start / end to playhead"),
    ("Enter", "edit the selected caption's text"),
    ("L", "loop the selected clip or caption"),
    ("Ctrl+wheel, + / −", "zoom; wheel scrolls"),
    ("Ctrl+Z / Ctrl+Shift+Z", "undo / redo"),
    ("Ctrl+S", "save project"),
    ("Ctrl+F", "find & replace"),
    ("F2", "show / hide captions sidebar"),
    ("Esc", "deselect / leave text box"),
];

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.hint = "";
        self.handle_keys(&ctx);
        if let Some(lvl) = self.with_transport(|t| std::mem::take(&mut t.level)) {
            self.level = lvl.max(self.level * 0.85);
        }

        egui::Panel::top("top").show(ui, |ui| {
            self.menu_bar(ui);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        // the hint is computed while drawing the timeline, so the status bar shows last frame's
        let hint = self.hint_prev;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(format!(
                        "{} clips · {} captions · {}{}",
                        self.state.clips.len(),
                        self.state.captions.len(),
                        fmt_clock(self.dur()),
                        if self.dirty { " · unsaved" } else { "" }
                    ));
                    if !hint.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new(hint).color(Color32::from_rgb(150, 200, 255)));
                    }
                });
            });
        });
        if self.show_sidebar {
            egui::Panel::right("captions").resizable(true).default_size(560.0).min_size(320.0).max_size(1100.0).show(ui, |ui| self.caption_list(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            self.timeline(ui);
            ui.add_space(6.0);
            self.preview(ui);
        });
        self.windows(&ctx);
        self.autosave();
        if self.hint != self.hint_prev {
            self.hint_prev = self.hint;
            ctx.request_repaint();
        }

        let title = format!(
            "Audio Editor — {}{}",
            self.loaded.as_ref().map_or("no file", |l| l.audio_path.rsplit('/').next().unwrap_or("")),
            if self.dirty { " *" } else { "" }
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        if self.is_playing() || self.progress.is_some() || self.drag.is_some() {
            ctx.request_repaint();
        }
    }
}

fn resolve_relative(project: &Path, audio: &str) -> String {
    let p = Path::new(audio);
    if p.is_absolute() || p.exists() {
        return audio.to_string();
    }
    project.parent().map(|d| d.join(p)).unwrap_or_else(|| p.to_path_buf()).to_string_lossy().into_owned()
}

fn nice_tick(raw: f64) -> f64 {
    for t in [0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0] {
        if t >= raw {
            return t;
        }
    }
    1200.0
}

fn fmt_tick(t: f64, tick: f64) -> String {
    let m = (t / 60.0).floor();
    let s = t - m * 60.0;
    if tick < 1.0 {
        format!("{}:{:05.2}", m, s)
    } else {
        format!("{}:{:02}", m, s.round() as u64)
    }
}
