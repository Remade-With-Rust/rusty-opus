#!/bin/bash
# libopus multistream oracle (needs gcc + libopus.so.0; Linux / WSL).
# Build the oracle and generate every config into out/; then, on any host:
#   RUSTY_OPUS_MS_ORACLE=<this dir>/out cargo test --release \
#     --test multistream_oracle -- --nocapture
# fixtures.sh + to_r16.py (step 4) make the committed fixtures/multistream;
# to_rff_r16.py the trimmed, FFmpeg-ordered references remade-ffmpeg uses.
# Generates in /tmp (the 9p mount is slow for small writes), then copies to out/.
set -e
HERE=$(pwd)
gcc -O2 -Wall -o /tmp/opus-oracle oracle.c /usr/lib/x86_64-linux-gnu/libopus.so.0 -lm
rm -rf /tmp/opus-oracle-out && mkdir -p /tmp/opus-oracle-out && cd /tmp/opus-oracle-out
O=/tmp/opus-oracle
: > configs.txt
run() { $O "$@" | tee -a configs.txt; }

# family 1, generic encoder with the Vorbis layout: every size x forced mode
for ch in 3 4 6 8; do
  run f1_${ch}ch_celt_20  $ch 1 celt   $((48000*ch)) 20
  run f1_${ch}ch_silk_20  $ch 1 silk   $((12000*ch)) 20
  run f1_${ch}ch_hyb_20   $ch 1 hybrid $((24000*ch)) 20
done
# family 1 5.1: frame sizes
for fm in 10 60; do
  run f1_6ch_celt_$fm  6 1 celt   288000 $fm
  run f1_6ch_silk_$fm  6 1 silk   72000  $fm
  run f1_6ch_hyb_$fm   6 1 hybrid 144000 $fm
done
run f1_8ch_celt_10  8 1 celt 384000 10
run f1_8ch_celt_5   8 1 celt 384000 5
run f1_3ch_celt_2.5 3 1 celt 192000 2.5
# family 1 through libopus's real surround encoder (what opusenc/ffmpeg
# produce): coupled streams CELT, mono streams free/forced.
run f1s_6ch_auto_20  6 1 s:auto   192000 20
run f1s_6ch_auto_lo  6 1 s:auto   48000  20
run f1s_6ch_hyb_20   6 1 s:hybrid 128000 20
run f1s_8ch_silk_20  8 1 s:silk   48000  20
run f1s_8ch_auto_60  8 1 s:auto   256000 60
run f1s_3ch_auto_10  3 1 s:auto   96000  10

# family 255: 6 outputs from 1 coupled + 2 mono streams; output 3 silent,
# outputs 4 and 5 both carry mono stream 2 (decoded channel 3).
M6="3 1 0 1 2 255 3 3"
for fm in 10 20 60; do
  run f255_6ch_celt_$fm 6 255 celt   192000 $fm $M6
  run f255_6ch_silk_$fm 6 255 silk   48000  $fm $M6
  run f255_6ch_hyb_$fm  6 255 hybrid 96000  $fm $M6
done
# family 255: 4 outputs, 2 mono streams, no coupling; mono stream 0 on two
# outputs, one silent output.
M4="2 0 1 255 0 1"
run f255_4ch_celt_20 4 255 celt   96000 20 $M4
run f255_4ch_silk_20 4 255 silk   32000 20 $M4
run f255_4ch_hyb_20  4 255 hybrid 64000 20 $M4
# family 255: 3 outputs, 2 coupled streams, decoded channel 3 (right of
# stream 1) never mapped.
M3="2 2 0 1 2 3"
DECODE_CH=3 run f255_3ch_celt_20 4 255 celt 128000 20 $M3


# Other decode rates and CBR (padding on the last stream), table only.
for fs in 8000 12000 16000 24000; do
  DEC_FS=$fs run f1s_6ch_auto_20_d$fs 6 1 s:auto 192000 20
  DEC_FS=$fs run f255_6ch_hyb_20_d$fs 6 255 hybrid 96000 20 $M6
done
CBR=1 run f1s_6ch_cbr_20 6 1 s:auto 192000 20
CBR=1 run f255_6ch_silk_cbr_20 6 255 silk 48000 20 $M6

rm -rf "$HERE/out" && mkdir -p "$HERE/out" && cp -r /tmp/opus-oracle-out/. "$HERE/out/"
