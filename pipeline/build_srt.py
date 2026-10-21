"""Build an SRT + transcript.json from chunks and either a {chunk id: text} map or hypotheses (uses "final", else "ctc")."""
import utils.transport
import utils.session
import json, sys

CHUNKS, TEXTS, OUT_SRT, OUT_JSON = sys.argv[1:5]
chunks = json.load(open(CHUNKS))
raw = json.load(open(TEXTS))
texts = raw if isinstance(raw, dict) else {str(h["id"]): h.get("final", h.get("ctc", "")) for h in raw}

def fmt(t):
    ms = round(t * 1000)
    return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d},{ms % 1000:03d}"

def split_text(text, max_chars=84):
    lines, cur = [], ""
    for w in text.split():
        if cur and len(cur) + 1 + len(w) > max_chars:
            lines.append(cur); cur = w
        else:
            cur = f"{cur} {w}".strip()
    return lines + ([cur] if cur else [])

speakers, cues, transcript = {}, [], []
for c in chunks:
    spk = speakers.setdefault(c["speaker"], f"Speaker {len(speakers) + 1}")
    text = (texts.get(str(c["id"])) or "").strip()
    transcript.append({**c, "speaker_label": spk, "text": text})
    if not text:
        continue
    parts = split_text(text)
    total = sum(len(p) for p in parts)
    t = c["start"]
    for p in parts:  # spread a long chunk's text over its duration by character share
        d = (c["end"] - c["start"]) * len(p) / total
        cues.append((t, t + d, f"[{spk}] {p}"))
        t += d

json.dump(transcript, open(OUT_JSON, "w"), indent=1, ensure_ascii=False)
with open(OUT_SRT, "w") as f:
    for i, (s, e, txt) in enumerate(cues, 1):
        f.write(f"{i}\n{fmt(s)} --> {fmt(e)}\n{txt}\n\n")
print(f"{len(cues)} cues; {sum(1 for x in transcript if not x['text'])} empty chunks")
