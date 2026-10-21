"""Transcribe every chunk with both NVIDIA Kinyarwanda NeMo models -> hypotheses JSON."""
import json, sys, soundfile as sf
import nemo.collections.asr as nemo_asr

WAV, CHUNKS, OUT = sys.argv[1:4]
audio, sr = sf.read(WAV, dtype="float32")
chunks = json.load(open(CHUNKS))
clips = [audio[int(c["clip_start"] * sr): int(c["clip_end"] * sr)] for c in chunks]
hyps = {c["id"]: {"id": c["id"], "start": c["start"], "end": c["end"], "speaker": c["speaker"]} for c in chunks}
for key, name in [("ctc", "stt_rw_conformer_ctc_large"), ("rnnt", "stt_rw_conformer_transducer_large")]:
    m = nemo_asr.models.ASRModel.from_pretrained(name).eval().cuda()
    out = m.transcribe(clips, batch_size=16, verbose=False)
    out = out[0] if isinstance(out, tuple) else out
    for c, o in zip(chunks, out):
        hyps[c["id"]][key] = getattr(o, "text", o)
    del m
json.dump(list(hyps.values()), open(OUT, "w"), indent=1, ensure_ascii=False)
print(len(hyps), "chunks transcribed")
