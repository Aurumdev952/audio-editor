//! Edit model. The timeline is a list of audio clips, each placing a range of the *source* audio
//! at a timeline position; gaps between clips are silence. Captions are stored in source time so
//! they travel with their audio when clips are split, moved or deleted.

use serde::{Deserialize, Serialize};

pub const MIN_CLIP: f64 = 0.05;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Caption {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
    pub text: String,
    /// English translation of `text`.
    #[serde(default)]
    pub translation: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SubLang {
    Original,
    Translation,
    Both,
}

impl SubLang {
    pub const ALL: [SubLang; 3] = [SubLang::Original, SubLang::Translation, SubLang::Both];

    pub fn label(self) -> &'static str {
        match self {
            SubLang::Original => "Kinyarwanda",
            SubLang::Translation => "English",
            SubLang::Both => "Kinyarwanda + English",
        }
    }

    /// Subtitle lines for a caption in this language mode.
    pub fn lines(self, c: &Caption) -> Vec<String> {
        let (t, tr) = (c.text.trim().to_string(), c.translation.trim().to_string());
        match self {
            SubLang::Original => vec![t],
            SubLang::Translation => vec![tr],
            SubLang::Both => vec![t, tr],
        }
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// Timeline position of the clip's start.
    pub at: f64,
    pub src_start: f64,
    pub src_end: f64,
}

impl Clip {
    pub fn len(&self) -> f64 {
        self.src_end - self.src_start
    }

    pub fn end(&self) -> f64 {
        self.at + self.len()
    }

    pub fn to_src(&self, t: f64) -> f64 {
        self.src_start + (t - self.at)
    }
}

/// A caption (or the part of it) that is visible inside one clip, in timeline time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub caption: usize,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EditState {
    /// Sorted by `at`, never overlapping.
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Legacy format (removed source ranges); converted to clips on load.
    #[serde(default, skip_serializing)]
    pub cuts: Vec<(f64, f64)>,
    /// Sorted by start, in source time.
    pub captions: Vec<Caption>,
}

#[derive(Serialize, Deserialize)]
pub struct ProjectFile {
    pub audio_path: String,
    pub state: EditState,
}

impl EditState {
    /// Make sure there is a clip list: one clip for the whole file, or the kept parts of a legacy project.
    pub fn ensure_clips(&mut self, src_dur: f64) {
        if !self.clips.is_empty() {
            return;
        }
        let mut cur = 0.0;
        let mut at = 0.0;
        let mut cuts = std::mem::take(&mut self.cuts);
        cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (a, b) in cuts.into_iter().chain([(src_dur, src_dur)]) {
            if a > cur {
                self.clips.push(Clip { at, src_start: cur, src_end: a });
                at += a - cur;
            }
            cur = cur.max(b);
        }
    }

    pub fn duration(&self) -> f64 {
        self.clips.iter().map(|c| c.end()).fold(0.0, f64::max)
    }

    pub fn clip_at(&self, t: f64) -> Option<usize> {
        self.clips.iter().position(|c| t >= c.at && t < c.end())
    }

    pub fn edit_to_src(&self, t: f64) -> Option<f64> {
        self.clip_at(t).map(|i| self.clips[i].to_src(t))
    }

    /// Source ranges audible in timeline interval [t0, t1).
    pub fn src_ranges(&self, t0: f64, t1: f64) -> Vec<(f64, f64)> {
        self.clips
            .iter()
            .filter(|c| c.end() > t0 && c.at < t1)
            .map(|c| (c.to_src(t0.max(c.at)), c.to_src(t1.min(c.end()))))
            .collect()
    }

    /// Empty stretch of timeline around `t` (between clips), if `t` is in one.
    pub fn gap_at(&self, t: f64) -> Option<(f64, f64)> {
        if self.clip_at(t).is_some() || t < 0.0 {
            return None;
        }
        let prev = self.clips.iter().filter(|c| c.end() <= t).map(|c| c.end()).fold(0.0, f64::max);
        let next = self.clips.iter().filter(|c| c.at > t).map(|c| c.at).fold(f64::INFINITY, f64::min);
        next.is_finite().then_some((prev, next))
    }

    /// Split the clip under `t` in two. Returns the index of the right-hand piece.
    pub fn split_at(&mut self, t: f64) -> Option<usize> {
        let i = self.clip_at(t)?;
        let c = self.clips[i];
        if t - c.at < MIN_CLIP || c.end() - t < MIN_CLIP {
            return None;
        }
        let s = c.to_src(t);
        self.clips[i].src_end = s;
        self.clips.insert(i + 1, Clip { at: t, src_start: s, src_end: c.src_end });
        Some(i + 1)
    }

    /// Remove a clip, leaving a gap.
    pub fn delete_clip(&mut self, i: usize) {
        self.clips.remove(i);
    }

    /// Remove a clip and pull everything after it left to close the space.
    pub fn ripple_delete(&mut self, i: usize) {
        let len = self.clips.remove(i).len();
        for n in &mut self.clips[i..] {
            n.at -= len;
        }
    }

    /// Close the gap containing `t`, pulling every later clip left.
    pub fn close_gap(&mut self, t: f64) -> bool {
        let Some((a, b)) = self.gap_at(t) else { return false };
        for c in self.clips.iter_mut().filter(|c| c.at >= b) {
            c.at -= b - a;
        }
        true
    }

    pub fn close_all_gaps(&mut self) {
        let mut at = 0.0;
        for c in &mut self.clips {
            c.at = at;
            at = c.end();
        }
    }

    /// Move clip `i` to start at `new_at`. Dropping onto another clip inserts before it (if over its
    /// first half) or after it, pushing later clips right. Returns the clip's new index.
    pub fn move_clip(&mut self, i: usize, new_at: f64) -> usize {
        let mut moving = self.clips.remove(i);
        let mut at = new_at.max(0.0);
        if let Some(o) = self.clips.iter().find(|o| at > o.at && at < o.end()) {
            at = if at < o.at + o.len() / 2.0 { o.at } else { o.end() };
        }
        moving.at = at;
        // insert before any clip starting at or after the drop point
        let pos = self.clips.partition_point(|o| o.at < at);
        self.clips.insert(pos, moving);
        for k in pos + 1..self.clips.len() {
            let prev_end = self.clips[k - 1].end();
            if self.clips[k].at < prev_end - 1e-9 {
                self.clips[k].at = prev_end;
            }
        }
        pos
    }

    /// Drag a clip's left edge to timeline time `t` (the right edge stays put).
    pub fn trim_start(&mut self, i: usize, t: f64) {
        let prev_end = if i > 0 { self.clips[i - 1].end() } else { 0.0 };
        let c = &mut self.clips[i];
        let t = t.clamp(prev_end.max(c.at - c.src_start), c.end() - MIN_CLIP);
        c.src_start += t - c.at;
        c.at = t;
    }

    /// Drag a clip's right edge to timeline time `t`.
    pub fn trim_end(&mut self, i: usize, t: f64, src_dur: f64) {
        let next_at = self.clips.get(i + 1).map_or(f64::INFINITY, |n| n.at);
        let c = &mut self.clips[i];
        let t = t.clamp(c.at + MIN_CLIP, next_at.min(c.at + (src_dur - c.src_start)));
        c.src_end = c.to_src(t);
    }

    /// Every visible part of every caption, in timeline time.
    pub fn pieces(&self) -> Vec<Piece> {
        let mut out = Vec::new();
        for (ci, cap) in self.captions.iter().enumerate() {
            for c in &self.clips {
                let (s0, s1) = (cap.start.max(c.src_start), cap.end.min(c.src_end));
                if s1 - s0 > 1e-3 {
                    out.push(Piece { caption: ci, start: c.at + (s0 - c.src_start), end: c.at + (s1 - c.src_start) });
                }
            }
        }
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        out
    }

    pub fn caption_at_edit(&self, t: f64) -> Option<usize> {
        let s = self.edit_to_src(t)?;
        self.captions.iter().position(|c| s >= c.start && s < c.end)
    }

    pub fn sort_captions(&mut self) {
        self.captions.sort_by(|x, y| x.start.total_cmp(&y.start));
    }

    pub fn speakers(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for c in &self.captions {
            if !v.contains(&c.speaker) {
                v.push(c.speaker.clone());
            }
        }
        v
    }

    /// Captions laid out on the timeline (what exports use); a caption cut by a split appears once per piece.
    pub fn edited_captions(&self) -> Vec<Caption> {
        self.pieces()
            .into_iter()
            .map(|p| Caption { start: p.start, end: p.end, ..self.captions[p.caption].clone() })
            .filter(|c| c.end - c.start > 0.01 && !(c.text.trim().is_empty() && c.translation.trim().is_empty()))
            .collect()
    }
}

pub fn fmt_srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

pub fn fmt_clock(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

fn parse_srt_time(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// "[Speaker 1] text" -> ("Speaker 1", "text")
fn split_speaker(line: &str) -> (String, String) {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(close) = rest.find(']') {
            return (rest[..close].to_string(), rest[close + 1..].trim().to_string());
        }
    }
    (String::new(), line.to_string())
}

pub fn parse_srt(src: &str) -> Vec<Caption> {
    let src = src.replace("\r\n", "\n");
    let mut out = Vec::new();
    for block in src.split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());
        let Some(mut first) = lines.next() else { continue };
        if !first.contains("-->") {
            match lines.next() {
                Some(l) => first = l,
                None => continue,
            }
        }
        let Some((a, b)) = first.split_once("-->") else { continue };
        let (Some(start), Some(end)) = (parse_srt_time(a), parse_srt_time(b)) else { continue };
        let text: Vec<&str> = lines.collect();
        let (speaker, text) = split_speaker(&text.join(" "));
        out.push(Caption { start, end, speaker, text, translation: String::new() });
    }
    out.sort_by(|x, y| x.start.total_cmp(&y.start));
    out
}

pub fn write_srt(caps: &[Caption], with_speaker: bool, lang: SubLang) -> String {
    let mut s = String::new();
    let mut n = 0;
    for c in caps {
        let lines = lang.lines(c);
        if lines.is_empty() {
            continue;
        }
        n += 1;
        let mut text = lines.join("\n");
        if with_speaker && !c.speaker.is_empty() {
            text = format!("[{}] {}", c.speaker, text);
        }
        s.push_str(&format!("{}\n{} --> {}\n{}\n\n", n, fmt_srt_time(c.start), fmt_srt_time(c.end), text));
    }
    s
}

/// Fill each caption's translation from the SRT cue that overlaps it most.
/// Returns how many captions were filled.
pub fn apply_translation_srt(captions: &mut [Caption], cues: &[Caption]) -> usize {
    let mut n = 0;
    for c in captions.iter_mut() {
        let best = cues
            .iter()
            .map(|q| (q, c.end.min(q.end) - c.start.max(q.start)))
            .filter(|(_, overlap)| *overlap > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((q, _)) = best {
            c.translation = q.text.clone();
            n += 1;
        }
    }
    n
}


#[cfg(test)]
mod tests {
    use super::*;

    fn cap(start: f64, end: f64, text: &str) -> Caption {
        Caption { start, end, speaker: "A".into(), text: text.into(), translation: String::new() }
    }

    fn whole(dur: f64) -> EditState {
        let mut s = EditState::default();
        s.ensure_clips(dur);
        s
    }

    #[test]
    fn legacy_cuts_become_packed_clips() {
        let mut s = EditState { cuts: vec![(10.0, 15.0), (25.0, 27.0)], ..Default::default() };
        s.ensure_clips(100.0);
        assert_eq!(
            s.clips,
            vec![
                Clip { at: 0.0, src_start: 0.0, src_end: 10.0 },
                Clip { at: 10.0, src_start: 15.0, src_end: 25.0 },
                Clip { at: 20.0, src_start: 27.0, src_end: 100.0 },
            ]
        );
        assert_eq!(s.duration(), 93.0);
    }

    #[test]
    fn split_delete_and_move_to_fill_the_gap() {
        // the CapCut workflow: split twice, delete the middle, drag the tail left
        let mut s = whole(30.0);
        s.split_at(10.0);
        s.split_at(20.0);
        assert_eq!(s.clips.len(), 3);
        s.delete_clip(1);
        assert_eq!(s.gap_at(15.0), Some((10.0, 20.0)));
        assert_eq!(s.edit_to_src(15.0), None);
        let i = s.move_clip(1, 10.0);
        assert_eq!(i, 1);
        assert_eq!(s.clips[1], Clip { at: 10.0, src_start: 20.0, src_end: 30.0 });
        assert_eq!(s.duration(), 20.0);
        assert_eq!(s.edit_to_src(12.0), Some(22.0));
    }

    #[test]
    fn ripple_delete_and_close_gap() {
        let mut s = whole(30.0);
        s.split_at(10.0);
        s.split_at(20.0);
        s.ripple_delete(1);
        assert_eq!(s.clips[1].at, 10.0);
        assert_eq!(s.duration(), 20.0);

        let mut s = whole(30.0);
        s.split_at(10.0);
        s.split_at(20.0);
        s.delete_clip(1);
        assert!(s.close_gap(15.0));
        assert_eq!(s.clips[1].at, 10.0);
    }

    #[test]
    fn dropping_onto_a_clip_inserts_and_pushes() {
        let mut s = whole(30.0);
        s.split_at(10.0);
        s.split_at(20.0);
        // drag the last clip onto the first half of the first clip -> becomes first, others pushed
        let i = s.move_clip(2, 2.0);
        assert_eq!(i, 0);
        assert_eq!(s.clips.iter().map(|c| (c.at, c.src_start)).collect::<Vec<_>>(), vec![(0.0, 20.0), (10.0, 0.0), (20.0, 10.0)]);
    }

    #[test]
    fn trimming_respects_neighbours_and_source() {
        let mut s = whole(30.0);
        s.split_at(10.0);
        s.trim_start(1, 5.0); // can't overlap the previous clip
        assert_eq!(s.clips[1].at, 10.0);
        s.trim_start(1, 12.0);
        assert_eq!((s.clips[1].at, s.clips[1].src_start), (12.0, 12.0));
        s.trim_end(0, 20.0, 30.0); // can't run into the next clip
        assert_eq!(s.clips[0].end(), 12.0);
    }

    #[test]
    fn captions_follow_their_audio() {
        let mut s = whole(30.0);
        s.captions = vec![cap(2.0, 4.0, "a"), cap(9.0, 11.0, "b"), cap(25.0, 27.0, "c")];
        s.split_at(10.0);
        s.split_at(20.0);
        s.delete_clip(1);
        s.move_clip(1, 10.0);
        let caps = s.edited_captions();
        let spans: Vec<(f64, f64, &str)> = caps.iter().map(|c| (c.start, c.end, c.text.as_str())).collect();
        assert_eq!(spans, vec![(2.0, 4.0, "a"), (9.0, 10.0, "b"), (15.0, 17.0, "c")]);
        assert_eq!(s.caption_at_edit(16.0), Some(2));
    }

    #[test]
    fn srt_round_trip() {
        let caps = vec![Caption { start: 1.5, end: 3.25, speaker: "Speaker 1".into(), text: "Muraho neza".into(), translation: String::new() }];
        assert_eq!(parse_srt(&write_srt(&caps, true, SubLang::Original)), caps);
    }

    #[test]
    fn bilingual_srt_and_translation_import() {
        let mut caps = vec![
            Caption { start: 0.0, end: 2.0, speaker: "A".into(), text: "Muraho".into(), translation: "Hello".into() },
            Caption { start: 3.0, end: 4.0, speaker: "B".into(), text: "".into(), translation: "".into() },
        ];
        assert_eq!(write_srt(&caps, false, SubLang::Both), "1\n00:00:00,000 --> 00:00:02,000\nMuraho\nHello\n\n");
        let cues = parse_srt("1\n00:00:00,100 --> 00:00:01,900\nHi there\n\n2\n00:00:03,200 --> 00:00:03,900\nYes\n");
        assert_eq!(apply_translation_srt(&mut caps, &cues), 2);
        assert_eq!((caps[0].translation.as_str(), caps[1].translation.as_str()), ("Hi there", "Yes"));
    }
}
