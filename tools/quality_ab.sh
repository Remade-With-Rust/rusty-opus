#!/bin/bash
# rusty-opus quality A/B: score OUR encoder vs libopus (C) at equal bitrate via
# PEAQ ODG. Settles whether a speed win costs quality, and arms the PEAQ gate for
# bitstream-moving bricks.
#
# Usage: tools/quality_ab.sh <input.wav> <bitrate_bps> <audio|voip>
# Requires: ffmpeg (libopus) on PATH; remade_ffmpeg_rs PEAQ harness.
set -e

IN=$1; BR=${2:-128000}; APP=${3:-audio}
RFF=c:/Users/talmo/coding/remade_ffmpeg_rs
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# OURS: fork encoder + fork decoder (both libopus-equivalent) -> wav
cargo run --release --example roundtrip -- "$IN" "$WORK/ours.wav" "$BR" "$APP" 2>/dev/null

# LIBOPUS: C encoder -> ogg -> C decoder -> wav (ffmpeg both legs)
ffmpeg -hide_banner -loglevel error -y -i "$IN" -c:a libopus -b:a "$BR" -application "$APP" "$WORK/c.opus"
ffmpeg -hide_banner -loglevel error -y -i "$WORK/c.opus" "$WORK/libopus.wav"

echo "--- PEAQ ODG (0 = transparent, more negative = worse) at ${BR} bps, $APP ---"
printf "  rusty-opus : "; python "$RFF/tools/quality/peaq_run.py" "$IN" "$WORK/ours.wav"    "$RFF/PEAQ_python"
printf "  libopus (C): "; python "$RFF/tools/quality/peaq_run.py" "$IN" "$WORK/libopus.wav" "$RFF/PEAQ_python"
