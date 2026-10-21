//! Decoding (via ffmpeg), waveform peaks and cpal playback that skips cut ranges.

use crate::model::Clip;
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub const SAMPLE_RATE: u32 = 48_000;

pub fn decode(path: &str) -> Result<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", path, "-f", "f32le", "-ac", "1", "-ar", &SAMPLE_RATE.to_string(), "-"])
        .output()
        .context("running ffmpeg (is it installed?)")?;
    if !out.status.success() {
        bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

/// Min/max pyramid for fast waveform drawing at any zoom.
pub struct Peaks {
    levels: Vec<(usize, Vec<(f32, f32)>)>, // (block size in samples, per-block min/max)
}

impl Peaks {
    pub fn new(samples: &[f32]) -> Self {
        let base = 32;
        let mut cur: Vec<(f32, f32)> = samples
            .chunks(base)
            .map(|c| c.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v))))
            .collect();
        let mut levels = vec![(base, cur.clone())];
        let mut block = base;
        while cur.len() > 1 {
            cur = cur.chunks(4).map(|c| c.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &(a, b)| (lo.min(a), hi.max(b)))).collect();
            block *= 4;
            levels.push((block, cur.clone()));
        }
        Peaks { levels }
    }

    pub fn range(&self, samples: &[f32], s0: usize, s1: usize) -> (f32, f32) {
        let s1 = s1.min(samples.len());
        if s1 <= s0 {
            return (0.0, 0.0);
        }
        let len = s1 - s0;
        if len < 64 {
            return samples[s0..s1].iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        }
        let (block, data) = self.levels.iter().rev().find(|(b, _)| *b * 2 <= len).unwrap_or(&self.levels[0]);
        let (b0, b1) = (s0 / block, (s1 / block).max(s0 / block + 1).min(data.len()));
        data[b0..b1].iter().fold((f32::MAX, f32::MIN), |(lo, hi), &(a, b)| (lo.min(a), hi.max(b)))
    }
}

pub struct Transport {
    pub playing: bool,
    /// Timeline position in seconds.
    pub pos: f64,
    pub speed: f32,
    pub volume: f32,
    /// Sorted by timeline position; gaps between clips play as silence.
    pub clips: Vec<Clip>,
    /// Timeline loop range.
    pub loop_range: Option<(f64, f64)>,
    /// Stop when reaching this timeline time.
    pub stop_at: Option<f64>,
    pub level: f32,
}

impl Transport {
    fn next_sample(&mut self, samples: &[f32], dt: f64) -> f32 {
        if !self.playing {
            return 0.0;
        }
        if let Some((a, b)) = self.loop_range {
            if self.pos >= b || self.pos < a - 0.05 {
                self.pos = a;
            }
        }
        if let Some(stop) = self.stop_at {
            if self.pos >= stop {
                self.playing = false;
                self.stop_at = None;
                return 0.0;
            }
        }
        let end = self.clips.last().map_or(0.0, |c| c.end());
        if self.pos >= end {
            self.playing = false;
            return 0.0;
        }
        let i = self.clips.partition_point(|c| c.end() <= self.pos);
        let v = match self.clips.get(i) {
            Some(c) if self.pos >= c.at => {
                let x = c.to_src(self.pos) * SAMPLE_RATE as f64;
                let i = x as usize;
                let frac = (x - i as f64) as f32;
                let a = samples.get(i).copied().unwrap_or(0.0);
                let b = samples.get(i + 1).copied().unwrap_or(a);
                (a + (b - a) * frac) * self.volume
            }
            _ => 0.0, // gap
        };
        self.pos += dt * self.speed as f64;
        v
    }
}

pub struct Player {
    _stream: cpal::Stream,
    pub transport: Arc<Mutex<Transport>>,
}

impl Player {
    pub fn new(samples: Arc<Vec<f32>>, clips: Vec<Clip>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().context("no audio output device")?;
        let supported = device.default_output_config()?;
        let config: cpal::StreamConfig = supported.config();
        let transport = Arc::new(Mutex::new(Transport {
            playing: false,
            pos: 0.0,
            speed: 1.0,
            volume: 1.0,
            clips,
            loop_range: None,
            stop_at: None,
            level: 0.0,
        }));
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, transport.clone(), samples)?,
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, transport.clone(), samples)?,
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, transport.clone(), samples)?,
            f => bail!("unsupported sample format {f:?}"),
        };
        stream.play()?;
        Ok(Player { _stream: stream, transport })
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    transport: Arc<Mutex<Transport>>,
    samples: Arc<Vec<f32>>,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    let dt = 1.0 / config.sample_rate.0 as f64;
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mut t = transport.lock().unwrap();
            let mut peak = 0.0f32;
            for frame in data.chunks_mut(channels) {
                let v = t.next_sample(&samples, dt);
                peak = peak.max(v.abs());
                for s in frame {
                    *s = T::from_sample(v);
                }
            }
            t.level = t.level.max(peak);
        },
        |e| eprintln!("audio stream error: {e}"),
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_clips_in_timeline_order_with_silent_gaps() {
        // source: 1 s of 0.25 then 1 s of 0.75; timeline: second second first, then a 1 s gap, then the first
        let sr = SAMPLE_RATE as usize;
        let samples: Vec<f32> = (0..2 * sr).map(|i| if i < sr { 0.25 } else { 0.75 }).collect();
        let clips = vec![Clip { at: 0.0, src_start: 1.0, src_end: 2.0 }, Clip { at: 2.0, src_start: 0.0, src_end: 1.0 }];
        let mut t = Transport { playing: true, pos: 0.0, speed: 1.0, volume: 1.0, clips, loop_range: None, stop_at: None, level: 0.0 };
        let dt = 1.0 / SAMPLE_RATE as f64;
        let out: Vec<f32> = (0..3 * sr + 10).map(|_| t.next_sample(&samples, dt)).collect();
        assert_eq!(out[sr / 2], 0.75);
        assert_eq!(out[sr + sr / 2], 0.0);
        assert_eq!(out[2 * sr + sr / 2], 0.25);
        assert!(!t.playing, "stops at the end of the last clip");
    }
}
