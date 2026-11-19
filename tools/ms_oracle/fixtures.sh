#!/bin/bash
# Small fixtures to commit to rusty-opus (tests/fixtures/multistream). Short
# clips; references decoded by libopus at 48 kHz.
set -e
HERE=$(pwd)
gcc -O2 -Wall -o /tmp/opus-oracle oracle.c /usr/lib/x86_64-linux-gnu/libopus.so.0 -lm
rm -rf /tmp/opus-oracle-fx && mkdir -p /tmp/opus-oracle-fx && cd /tmp/opus-oracle-fx
O=/tmp/opus-oracle
run() { $O "$@" | tee -a configs.txt; }
: > configs.txt
# 5.1 from libopus's surround encoder (coupled CELT + mono hybrid), 20 ms.
DUR_MS=60 run ms_f1_6ch_surround_20 6 1 s:auto 192000 20
# 7.1, 60 ms code-3 packets, CELT.
DUR_MS=60 run ms_f1_8ch_celt_60 8 1 celt 256000 60
# 3.0 SILK through the generic encoder: bit-exact expected.
DUR_MS=120 run ms_f1_3ch_silk_20 3 1 silk 36000 20
# family 255: silent output + one mono stream on two outputs, hybrid 10 ms.
DUR_MS=60 run ms_f255_6ch_hyb_10 6 255 hybrid 96000 10 3 1 0 1 2 255 3 3
# CBR: padding on the last stream.
CBR=1 DUR_MS=40 run ms_f255_4ch_silk_cbr_20 4 255 silk 32000 20 2 0 1 255 0 1
# (.opus kept: the rff end-to-end fixtures)
rm -rf "$HERE/fx" && mkdir -p "$HERE/fx" && cp -r /tmp/opus-oracle-fx/. "$HERE/fx/"
