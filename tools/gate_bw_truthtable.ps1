# Great Gate P1: validate the analysis bandwidth DETECTOR against known truth.
#
# The P2 narrowing gate would key on `detected_bandwidth`. A gate fitted on a
# lying signal fits the lie, so the signal is validated against a per-class
# truth table FIRST (great-gate.md §2: validate against a brute-force oracle or
# per-class truth table BEFORE wiring).
#
# Method: low-pass the same source at known cutoffs, encode with the harvest
# tap on, and read the modal `det_bw` the analysis reported. Expected mapping
# (libopus analysis.c band edges): 4k->NB(1101) 6k->MB(1102) 8k->WB(1103)
# 12k->SWB(1104) full->FB(1105).
#
#   powershell tools\gate_bw_truthtable.ps1
param([string]$Source = "speech_clean", [int]$Rate = 32000)

$ROOT = Split-Path -Parent $PSScriptRoot
$RT = Join-Path $ROOT "target\release\examples\roundtrip.exe"
$C = Join-Path $ROOT "fixtures\gate_corpus"
$W = Join-Path $ROOT "target\bw_truth"
New-Item -ItemType Directory -Force $W | Out-Null
$csv = Join-Path $W "harvest.csv"
Remove-Item $csv -ErrorAction SilentlyContinue -Confirm:$false

$env:RUSTY_OPUS_FORCE_MODE = $null
$env:RUSTY_OPUS_GATE_HARVEST = $csv

$cutoffs = @(4000, 6000, 8000, 12000, 16000, 20000)
foreach ($cut in $cutoffs) {
    $lp = Join-Path $W "lp$cut.wav"
    # 4th-order low-pass, applied twice for a steeper skirt so the "true"
    # bandwidth is unambiguous rather than a gentle roll-off.
    & ffmpeg -hide_banner -loglevel error -y -i (Join-Path $C "$Source.wav") `
        -af "lowpass=f=$cut`:poles=2,lowpass=f=$cut`:poles=2" -ar 48000 -sample_fmt s16 $lp
    $env:RUSTY_OPUS_GATE_CLIP = "lp$cut"
    & $RT $lp (Join-Path $W "out$cut.wav") $Rate audio 2>&1 | Out-Null
}
# Full-band anchor (no filtering).
$env:RUSTY_OPUS_GATE_CLIP = "unfiltered"
& $RT (Join-Path $C "$Source.wav") (Join-Path $W "out_full.wav") $Rate audio 2>&1 | Out-Null
$env:RUSTY_OPUS_GATE_HARVEST = $null
$env:RUSTY_OPUS_GATE_CLIP = $null

Write-Host "`n=== bandwidth detector truth table (source: $Source @ $($Rate/1000)k) ==="
Write-Host ("{0,-12} {1,-14} {2,-14} {3}" -f "input LP", "expected", "modal det_bw", "coded bw (modal)")
$names = @{ "1101" = "NB(1101)"; "1102" = "MB(1102)"; "1103" = "WB(1103)"; "1104" = "SWB(1104)"; "1105" = "FB(1105)" }
$expect = @{ "lp4000" = "NB(1101)"; "lp6000" = "MB(1102)"; "lp8000" = "WB(1103)";
             "lp12000" = "SWB(1104)"; "lp16000" = "FB(1105)"; "lp20000" = "FB(1105)";
             "unfiltered" = "FB(1105)" }
$rows = Import-Csv $csv
foreach ($clip in @("lp4000", "lp6000", "lp8000", "lp12000", "lp16000", "lp20000", "unfiltered")) {
    $sel = $rows | Where-Object { $_.clip -eq $clip }
    if (-not $sel) { continue }
    $modalDet = ($sel | Group-Object det_bw | Sort-Object Count -Descending)[0]
    $modalBw = ($sel | Group-Object bw | Sort-Object Count -Descending)[0]
    $d = $names[$modalDet.Name]; if (-not $d) { $d = $modalDet.Name }
    $b = $names[$modalBw.Name]; if (-not $b) { $b = $modalBw.Name }
    $pct = [int](100 * $modalDet.Count / $sel.Count)
    $flag = if ($d -eq $expect[$clip]) { "" } else { "   <-- MISMATCH" }
    Write-Host ("{0,-12} {1,-14} {2,-14} {3}{4}" -f $clip, $expect[$clip], "$d ($pct%)", $b, $flag)
}
