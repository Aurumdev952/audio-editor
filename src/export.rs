//! Rendering the edited audio and the black-screen caption video through ffmpeg.

use crate::audio::SAMPLE_RATE;
use crate::model::{Caption, EditState, SubLang};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct VideoOptions {
    pub width: u32,
    pub height: u32,
    pub font_size: u32,
    pub show_speaker: bool,
    pub color_by_speaker: bool,
    pub lang: SubLang,
}

impl Default for VideoOptions {
    fn default() -> Self {
        VideoOptions { width: 1920, height: 1080, font_size: 64, show_speaker: true, color_by_speaker: true, lang: SubLang::Both }
    }
}

#[derive(Default)]
pub struct Progress {
    pub label: String,
    pub fraction: f32,
    pub done: Option<Result<String, String>>,
}

pub type ProgressHandle = Arc<Mutex<Progress>>;

const FADE_SECS: f64 = 0.008;

/// Lay the clips out on the timeline (gaps are silence), fading clip edges briefly so joins don't click.
/// Edges where the neighbouring clip continues the same source audio are left untouched.
pub fn render_edited(samples: &[f32], state: &EditState) -> Vec<f32> {
    let sr = SAMPLE_RATE as f64;
    let fade = (FADE_SECS * sr) as usize;
    let mut out = vec![0.0f32; (state.duration() * sr).round() as usize];
    let clips = &state.clips;
    for (k, c) in clips.iter().enumerate() {
        let joined_before = k > 0 && (clips[k - 1].end() - c.at).abs() < 1e-6 && (clips[k - 1].src_end - c.src_start).abs() < 1e-6;
        let joined_after = clips.get(k + 1).is_some_and(|n| (c.end() - n.at).abs() < 1e-6 && (c.src_end - n.src_start).abs() < 1e-6);
        let (i0, i1) = ((c.src_start * sr).round() as usize, ((c.src_end * sr).round() as usize).min(samples.len()));
        let o0 = (c.at * sr).round() as usize;
        let len = i1.saturating_sub(i0);
        for j in 0..len {
            let mut g = 1.0;
            if !joined_before && j < fade {
                g = j as f32 / fade as f32;
            }
            if !joined_after && len - j <= fade {
                g = f32::min(g, (len - j) as f32 / fade as f32);
            }
            if let Some(o) = out.get_mut(o0 + j) {
                *o += samples[i0 + j] * g;
            }
        }
    }
    out
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    Ok(())
}

fn temp_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("audio-editor-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Run ffmpeg, reporting progress (0..1) against `total_secs`.
fn run_ffmpeg(args: &[String], cwd: &Path, total_secs: f64, progress: &ProgressHandle) -> Result<()> {
    let mut child = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-progress", "pipe:1", "-nostats"])
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting ffmpeg")?;
    let stdout = child.stdout.take().unwrap();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
            progress.lock().unwrap().fraction = (us / 1e6 / total_secs).clamp(0.0, 1.0) as f32;
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

fn encoder_args(out: &Path) -> Vec<String> {
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let v: &[&str] = match ext.as_str() {
        "wav" => &["-c:a", "pcm_s16le"],
        "mp3" => &["-c:a", "libmp3lame", "-q:a", "2"],
        "flac" => &["-c:a", "flac"],
        "ogg" | "opus" => &["-c:a", "libopus", "-b:a", "96k"],
        _ => &["-c:a", "aac", "-b:a", "192k"],
    };
    v.iter().map(|s| s.to_string()).collect()
}

pub fn export_audio(samples: &[f32], state: &EditState, out: &Path, progress: &ProgressHandle) -> Result<()> {
    let edited = render_edited(samples, state);
    let dir = temp_dir()?;
    let wav = dir.join("edited.wav");
    write_wav(&wav, &edited)?;
    let mut args = vec!["-i".into(), wav.to_string_lossy().into_owned()];
    args.extend(encoder_args(out));
    args.push(out.to_string_lossy().into_owned());
    run_ffmpeg(&args, &dir, edited.len() as f64 / SAMPLE_RATE as f64, progress)
}

const SPEAKER_COLORS: [&str; 6] = ["&H00FFFFFF", "&H0066E0FF", "&H00FFC870", "&H0090FF90", "&H00C890FF", "&H007070FF"];

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

pub fn build_ass(caps: &[Caption], speakers: &[String], o: &VideoOptions) -> String {
    let mut s = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
        w = o.width,
        h = o.height
    );
    let style_count = if o.color_by_speaker { speakers.len().max(1) } else { 1 };
    for i in 0..style_count {
        s.push_str(&format!(
            "Style: S{i},DejaVu Sans,{fs},{c},&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,5,{m},{m},40,1\n",
            fs = o.font_size,
            c = SPEAKER_COLORS[i % SPEAKER_COLORS.len()],
            m = o.width / 12
        ));
    }
    s.push_str("\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    for c in caps {
        let style = if o.color_by_speaker { speakers.iter().position(|x| *x == c.speaker).unwrap_or(0) } else { 0 };
        let lines: Vec<String> = o.lang.lines(c).iter().map(|l| l.replace('\n', " ").replace('{', "(").replace('}', ")")).collect();
        if lines.is_empty() {
            continue;
        }
        // in bilingual mode the English line is smaller, italic and pale yellow
        let body = match lines.as_slice() {
            [a, b] => format!("{a}\\N{{\\fs{}\\i1\\c&H00A0F0FF&}}{b}", o.font_size * 8 / 10),
            _ => lines.join("\\N"),
        };
        let text = if o.show_speaker && !c.speaker.is_empty() {
            format!("{{\\fs{}\\alpha&H40&}}{}\\N{{\\r}}{}", o.font_size * 6 / 10, c.speaker, body)
        } else {
            body
        };
        s.push_str(&format!("Dialogue: 0,{},{},S{style},,0,0,0,,{text}\n", ass_time(c.start), ass_time(c.end)));
    }
    s
}

/// Longest a single video frame may last; keeps players' seeking responsive on long silent stretches.
const MAX_FRAME_SECS: f64 = 2.0;

/// The picture only changes when a caption starts or ends, so the video gets one frame per change
/// (variable frame rate) instead of 25 identical frames a second. Times are rounded to centiseconds,
/// the precision `ass_time` writes, so every frame lands exactly on a caption boundary.
fn frame_times(caps: &[Caption], dur: f64) -> Vec<(f64, f64)> {
    let cs = |t: f64| (t * 100.0).round() / 100.0;
    let mut cuts: Vec<f64> = caps.iter().flat_map(|c| [cs(c.start), cs(c.end)]).filter(|&t| t > 0.0 && t < dur).collect();
    cuts.push(0.0);
    cuts.push(dur);
    cuts.sort_by(f64::total_cmp);
    cuts.dedup();
    let mut frames = Vec::new();
    for w in cuts.windows(2) {
        let mut t = w[0];
        while w[1] - t > MAX_FRAME_SECS {
            frames.push((t, MAX_FRAME_SECS));
            t += MAX_FRAME_SECS;
        }
        if w[1] - t > 1e-4 {
            frames.push((t, w[1] - t));
        }
    }
    frames
}

pub fn export_video(samples: &[f32], state: &EditState, out: &Path, o: &VideoOptions, progress: &ProgressHandle) -> Result<()> {
    let edited = render_edited(samples, state);
    let dur = edited.len() as f64 / SAMPLE_RATE as f64;
    let dir = temp_dir()?;
    write_wav(&dir.join("edited.wav"), &edited)?;
    let caps = state.edited_captions();
    std::fs::write(dir.join("subs.ass"), build_ass(&caps, &state.speakers(), o))?;

    // concat-demuxer list: one black still per caption change, each held until the next change
    let mut list = String::new();
    for (_, d) in frame_times(&caps, dur) {
        list.push_str(&format!("file 'black.png'\nduration {d:.4}\n"));
    }
    list.push_str("file 'black.png'\n"); // the demuxer ignores the last entry's duration
    std::fs::write(dir.join("frames.txt"), list)?;
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    run_ffmpeg(&args(&["-f", "lavfi", "-i", &format!("color=c=black:s={}x{}", o.width, o.height), "-frames:v", "1", "black.png"]), &dir, 1.0, &ProgressHandle::default())?;

    // AAC encoding is single-threaded and the slowest step, so it runs alongside the video encode
    let audio = {
        let (dir, progress) = (dir.clone(), progress.clone());
        std::thread::spawn(move || run_ffmpeg(&args(&["-i", "edited.wav", "-c:a", "aac", "-b:a", "128k", "audio.m4a"]), &dir, dur, &progress))
    };
    run_ffmpeg(
        &args(&[
            "-f", "concat", "-safe", "0", "-i", "frames.txt",
            "-vf", "ass=subs.ass", "-fps_mode", "vfr",
            "-c:v", "libx264", "-preset", "veryfast", "-tune", "stillimage", "-pix_fmt", "yuv420p", "-crf", "23",
            "-an", "video.mp4",
        ]),
        &dir,
        dur,
        &ProgressHandle::default(),
    )?;
    audio.join().map_err(|_| anyhow::anyhow!("audio encoder thread panicked"))??;

    let mut mux = args(&["-i", "video.mp4", "-i", "audio.m4a", "-map", "0:v", "-map", "1:a", "-c", "copy", "-t", &format!("{dur:.3}"), "-movflags", "+faststart"]);
    mux.push(out.to_string_lossy().into_owned());
    run_ffmpeg(&mux, &dir, dur, &ProgressHandle::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Caption;

    #[test]
    fn video_frames_land_on_caption_changes() {
        let caps = vec![
            Caption { start: 0.5, end: 1.004, speaker: "A".into(), text: "a".into(), translation: String::new() },
            Caption { start: 1.004, end: 6.0, speaker: "A".into(), text: "b".into(), translation: String::new() },
        ];
        let f = frame_times(&caps, 7.0);
        let starts: Vec<f64> = f.iter().map(|x| (x.0 * 100.0).round() / 100.0).collect();
        assert_eq!(starts, vec![0.0, 0.5, 1.0, 3.0, 5.0, 6.0]);
        assert!((f.iter().map(|x| x.1).sum::<f64>() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn exports_audio_and_video_with_cuts() {
        let sr = SAMPLE_RATE as usize;
        let samples: Vec<f32> = (0..sr * 4).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / sr as f32).sin() * 0.3).collect();
        let mut state = EditState {
            captions: vec![
                Caption { start: 0.2, end: 1.5, speaker: "Speaker 1".into(), text: "Muraho".into(), translation: "Hello".into() },
                Caption { start: 2.5, end: 3.8, speaker: "Speaker 2".into(), text: "Amakuru?".into(), translation: "How are you?".into() },
            ],
            ..Default::default()
        };
        state.ensure_clips(4.0);
        state.split_at(1.0);
        state.split_at(2.0);
        state.ripple_delete(1);
        assert_eq!(render_edited(&samples, &state).len(), sr * 3);
        let dir = std::env::temp_dir().join("audio-editor-test");
        std::fs::create_dir_all(&dir).unwrap();
        let progress: ProgressHandle = Default::default();
        export_audio(&samples, &state, &dir.join("a.m4a"), &progress).unwrap();
        export_video(&samples, &state, &dir.join("v.mp4"), &VideoOptions::default(), &progress).unwrap();
        for f in ["a.m4a", "v.mp4"] {
            let out = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
                .arg(dir.join(f))
                .output()
                .unwrap();
            let d: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
            assert!((d - 3.0).abs() < 0.1, "{f}: {d}");
        }
    }

    /// `BENCH_PROJECT=path/to/project.json cargo test --release -- --ignored --nocapture bench_video_export`
    #[test]
    #[ignore]
    fn bench_video_export() {
        let path = std::path::PathBuf::from(std::env::var("BENCH_PROJECT").expect("set BENCH_PROJECT"));
        let pf: crate::model::ProjectFile = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let audio = path.parent().unwrap().join(&pf.audio_path);
        let samples = crate::audio::decode(audio.to_str().unwrap()).unwrap();
        let mut state = pf.state;
        state.ensure_clips(samples.len() as f64 / SAMPLE_RATE as f64);
        let out = std::env::temp_dir().join("audio-editor-bench.mp4");
        let t = std::time::Instant::now();
        export_video(&samples, &state, &out, &VideoOptions::default(), &ProgressHandle::default()).unwrap();
        println!("video export of {:.0}s took {:.1}s -> {}", state.duration(), t.elapsed().as_secs_f64(), out.display());
    }
}
