# Great Gate: default-path byte-identical proof.
#
# Hashes roundtrip output over the corpus x rate matrix so an edit batch can be
# proven output-neutral (codec-measurement par.0: gate BEFORE the number).
# Run once on the pre-edit binary (-Out reference.txt), once after the rebuild,
# then diff. The hygiene batch of 2026-08-07 touches only opt-in/FEC-on/loss>0
# paths + env caching, so EVERY row here must match.
#
#   powershell tools\gate_hash_matrix.ps1 -Out target\hashes_pre.txt
param([string]$Out = "target\hashes.txt")

$ROOT = Split-Path -Parent $PSScriptRoot
$RT = Join-Path $ROOT "target\release\examples\roundtrip.exe"
$C = Join-Path $ROOT "fixtures\gate_corpus"
$W = Join-Path $ROOT "target\hash_matrix_tmp"
New-Item -ItemType Directory -Force $W | Out-Null

# Never let an ambient harvest/force env leak into the proof.
$env:RUSTY_OPUS_GATE_HARVEST = $null
$env:RUSTY_OPUS_GATE_CLIP = $null
$env:RUSTY_OPUS_FORCE_MODE = $null

$lines = @()
$lines += "# roundtrip.exe mtime: " + (Get-Item $RT).LastWriteTimeUtc.ToString("o")
foreach ($f in (Get-ChildItem "$C\*.wav" | Sort-Object Name)) {
    foreach ($br in 24, 64, 128) {
        $o = Join-Path $W "$($f.BaseName)_$br.wav"
        $log = & $RT $f.FullName $o ($br * 1000) audio 2>&1 | Out-String
        $bytes = if ($log -match 'encoded (\d+) bytes') { $Matches[1] } else { "ERR" }
        $h = (Get-FileHash $o -Algorithm SHA256).Hash.Substring(0, 16)
        $lines += "{0,-22} {1,4}k  bytes={2,-8} sha={3}" -f $f.BaseName, $br, $bytes, $h
    }
}
$outPath = Join-Path $ROOT $Out
$lines | Out-File $outPath -Encoding utf8
Write-Host "wrote $outPath ($($lines.Count - 1) rows)"
