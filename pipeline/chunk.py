"""Cut a recording into bite-sized chunks: speaker turns from diarization, long turns split at the quietest point."""
import json, sys
import numpy as np
import soundfile as sf

WAV, SEGS, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
MERGE_GAP = 0.5      # same-speaker segments closer than this become one turn
MIN_LEN, MAX_LEN = 1.5, 10.0
PAD = 0.15
HOP = 0.01

audio, sr = sf.read(WAV, dtype="float32")
hop = int(sr * HOP)
frames = len(audio) // hop
rms = np.sqrt(np.mean(audio[: frames * hop].reshape(frames, hop) ** 2, axis=1) + 1e-12)
smooth = np.convolve(rms, np.ones(20) / 20, mode="same")  # 200 ms window

segs = sorted(json.load(open(SEGS)), key=lambda s: s["start"])
turns = []
for s in segs:
    t = turns[-1] if turns else None
    if t and t["speaker"] == s["speaker"] and s["start"] - t["end"] < MERGE_GAP:
        t["end"] = max(t["end"], s["end"])
    else:
        turns.append(dict(s))

def quietest(a, b):
    i, j = int(a / HOP), int(b / HOP)
    return (i + int(np.argmin(smooth[i:j]))) * HOP

chunks = []
for t in turns:
    start, end = t["start"], t["end"]
    while end - start > MAX_LEN:
        cut = quietest(start + MAX_LEN * 0.4, start + MAX_LEN)
        chunks.append({"start": start, "end": cut, "speaker": t["speaker"]})
        start = cut
    chunks.append({"start": start, "end": end, "speaker": t["speaker"]})

# fold tiny fragments into a same-speaker neighbour when adjacent
merged = []
for c in chunks:
    p = merged[-1] if merged else None
    if p and c["end"] - c["start"] < MIN_LEN and p["speaker"] == c["speaker"] and c["start"] - p["end"] < 1.0 and c["end"] - p["start"] <= MAX_LEN * 1.3:
        p["end"] = c["end"]
    else:
        merged.append(c)

dur = len(audio) / sr
for i, c in enumerate(merged):
    c["id"] = i
    c["clip_start"] = max(0.0, c["start"] - PAD)
    c["clip_end"] = min(dur, c["end"] + PAD)
json.dump(merged, open(OUT, "w"), indent=1)
lens = [c["end"] - c["start"] for c in merged]
print(f"{len(turns)} turns -> {len(merged)} chunks; len min {min(lens):.2f} med {np.median(lens):.2f} max {max(lens):.2f}; speech {sum(lens):.0f}s of {dur:.0f}s")
