#!/bin/bash
# Deterministic instruction-count campaign instrument (callgrind Ir).
#
#   tools/ir/measure.sh <label> [base-label]
#
# Builds examples/ir_bench with the bench profile (release codegen, no overflow
# checks) plus line tables, runs five encode and five decode instruments under
# callgrind in parallel, and prints each total beside its work-parity ANCHOR.
# With a base label it prints the per-instrument delta and FAILS (exit 1) if any
# anchor moved: a moved anchor means the experiment changed, not the code.
#
# Linux only (valgrind); on Windows run it through WSL, always with the same
# invocation shape, because the count carries a fixed per-invocation pedestal:
#   MSYS_NO_PATHCONV=1 wsl -e bash /mnt/<drive>/<repo>/tools/ir/measure.sh <label> [base]
set -u
export PATH="$HOME/.cargo/bin:$PATH"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
RUNS="${IR_RUNS:-$HOME/ir-runs}"
export CARGO_TARGET_DIR="${IR_TARGET:-$HOME/ir-target-repo}"
LABEL="${1:?label}"
BASE="${2:-}"
O="$RUNS/$LABEL"
mkdir -p "$O"
cd "$REPO" || exit 2
if ! CARGO_PROFILE_BENCH_DEBUG=1 cargo build --profile bench --example ir_bench >"$O/build.log" 2>&1; then
  tail -30 "$O/build.log"
  echo "BUILD FAILED"
  exit 2
fi
B="$CARGO_TARGET_DIR/release/examples/ir_bench"
F="$REPO/fixtures"
NAMES="silk hybrid celt_st celt_mono mixed"
declare -A ENC=(
  [silk]="$F/answer_16k.wav 24000 voip 9"
  [hybrid]="$F/gate_corpus/voip_speech_noisy.wav 16000 voip 9"
  [celt_st]="$F/gate_corpus/mus_guitar_st.wav 128000 audio 9"
  [celt_mono]="$F/gate_corpus/mus_fast_dense.wav 64000 audio 9"
  [mixed]="$F/gate_corpus/speech_clean.wav 32000 voip 9"
)
declare -A DEC=([silk]="16000 1" [hybrid]="48000 1" [celt_st]="48000 2" [celt_mono]="48000 1" [mixed]="48000 1")
# Fixed scratch path for the packet files, so argv is identical across labels.
P="$RUNS/.packets"
mkdir -p "$P"
for s in $NAMES; do
  "$B" enc ${ENC[$s]} "$P/$s.pk" >"$O/$s.enc.anchor" || { echo "ENC FAILED $s"; exit 2; }
  "$B" dec "$P/$s.pk" ${DEC[$s]} >"$O/$s.dec.anchor" || { echo "DEC FAILED $s"; exit 2; }
done
cd "$P" || exit 2
for s in $NAMES; do
  valgrind --tool=callgrind --callgrind-out-file="$O/$s.enc.cg" "$B" enc ${ENC[$s]} >/dev/null 2>"$O/$s.enc.log" &
  valgrind --tool=callgrind --callgrind-out-file="$O/$s.dec.cg" "$B" dec "$P/$s.pk" ${DEC[$s]} >/dev/null 2>"$O/$s.dec.log" &
done
wait
: >"$O/summary.txt"
for s in $NAMES; do
  for d in enc dec; do
    ir=$(grep -oP 'Collected : \K[0-9]+' "$O/$s.$d.log")
    echo "$s.$d $ir $(cat "$O/$s.$d.anchor")" >>"$O/summary.txt"
  done
done
python3 "$REPO/tools/ir/diff.py" "$O/summary.txt" ${BASE:+"$RUNS/$BASE/summary.txt"}
