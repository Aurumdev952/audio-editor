#!/usr/bin/env bash
# Diarize + transcribe a Kinyarwanda recording into a speaker-labelled SRT.
# Usage: pipeline/run.sh <audio file> <output dir>
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
audio="$1"
out="$2"
py="${PYTHON:-python}"
mkdir -p "$out"
ffmpeg -v error -y -i "$audio" -ac 1 -ar 16000 "$out/audio_16k.wav"
"$py" "$here/diarize.py" "$out/audio_16k.wav" "$out/segments.json"
"$py" "$here/chunk.py" "$out/audio_16k.wav" "$out/segments.json" "$out/chunks.json"
"$py" "$here/transcribe_nemo.py" "$out/audio_16k.wav" "$out/chunks.json" "$out/asr_hypotheses.json"
"$py" "$here/build_srt.py" "$out/chunks.json" "$out/asr_hypotheses.json" "$out/captions.srt" "$out/transcript.json"
echo "Done: $out/captions.srt"
