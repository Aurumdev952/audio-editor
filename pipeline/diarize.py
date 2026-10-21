"""Speaker diarization with NVIDIA Nemotron 3 Diarization. Usage: diarize.py <16k mono wav> <segments.json>"""
import json, sys
from nemo.collections.asr.models import SortformerEncLabelModel

audio, out = sys.argv[1], sys.argv[2]
model = SortformerEncLabelModel.from_pretrained("nvidia/Nemotron-3-Diarization")
model.eval()
mods = model.sortformer_modules
mods.spkcache_len = 264
mods.fifo_len = 40
mods.chunk_len = 340
mods.chunk_right_context = 40
mods.spkcache_update_period = 300
model._check_streaming_parameters()
segments = model.diarize(audio=[audio], batch_size=1)
rows = []
for line in segments[0]:
    s, e, spk = line.split()
    rows.append({"start": float(s), "end": float(e), "speaker": spk})
rows.sort(key=lambda r: r["start"])
json.dump(rows, open(out, "w"), indent=1)
print(len(rows), "segments;", sorted({r["speaker"] for r in rows}))
