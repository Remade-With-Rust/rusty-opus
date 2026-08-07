# Great Gate P2: collect the SPEED PAIR for every (clip, rate, arm) unit.
#
# Two builds, by law (codec-measurement par.6 + par.15):
#   --features profile -> deterministic stage CALL COUNTS   => work   (primary)
#   plain release      -> pinned best-of-N encode time      => cpu_ms (confirmatory)
# The profiled build's clock is taxed by its own rdtsc pairs, so its ms is
# discarded; the plain build has no counter, so its work is discarded.
#
# Emits target\p2_speedpair.csv: clip,rate_kbps,arm,work,cpu_ms
# (raw per-arm values; gate_p2_harvest.py differences them against arm=auto so
# positive = work/time SAVED by firing the gate.)
#
#   powershell tools\gate_speedpair.ps1
param(
    [string[]]$Clips = @('speech_clean', 'mixed_speech_music', 'mus_guitar'),
    [int[]]$Rates = @(24, 32, 48),
    [string[]]$Arms = @('auto', 'silk', 'celt', 'hybrid'),
    [int]$Passes = 7
)

$ROOT = Split-Path -Parent $PSScriptRoot
$C = Join-Path $ROOT "fixtures\gate_corpus"
$OUT = Join-Path $ROOT "target\p2_speedpair.csv"

Write-Host "building both arms of the instrument..."
& cargo build --release --example gate_arm_cost --manifest-path (Join-Path $ROOT "Cargo.toml") 2>&1 | Select-String -Pattern 'error|Finished'
$PLAIN = Join-Path $ROOT "target\release\examples\gate_arm_cost.exe"
$plainStamp = (Get-Item $PLAIN).LastWriteTimeUtc
# The profiled build goes to its own target dir so it cannot clobber the plain
# exe (and so neither rebuild invalidates the other every invocation).
$PROFDIR = Join-Path $ROOT "target\profbuild"
& cargo build --release --features profile --example gate_arm_cost `
    --manifest-path (Join-Path $ROOT "Cargo.toml") --target-dir $PROFDIR 2>&1 |
    Select-String -Pattern 'error|Finished'
$PROF = Join-Path $PROFDIR "release\examples\gate_arm_cost.exe"
if ((Get-Item $PLAIN).LastWriteTimeUtc -ne $plainStamp) {
    Write-Warning "plain exe was relinked by the profiled build - timings and counts may disagree"
}

function Get-Field($line, $key) {
    # Build the pattern by concatenation: interpolating it into a double-quoted
    # string makes the parser choke on the bracket expression.
    $pat = [regex]::Escape($key) + '=([0-9.]+)'
    if ($line -match $pat) { return $Matches[1] }
    return "NA"
}

$rows = @()
foreach ($clip in $Clips) {
    $wav = Join-Path $C "$clip.wav"
    foreach ($rate in $Rates) {
        foreach ($arm in $Arms) {
            if ($arm -eq 'auto') { $env:RUSTY_OPUS_FORCE_MODE = $null }
            else { $env:RUSTY_OPUS_FORCE_MODE = $arm }

            # work: profiled build, single deterministic pass (passes=1)
            $wLine = & $PROF $wav ($rate * 1000) 1 | Select-String 'ARMCOST'
            $work = Get-Field "$wLine" 'work'

            # cpu_ms: plain build, pinned to one core at High priority, best-of-N
            $psi = New-Object System.Diagnostics.ProcessStartInfo
            $psi.FileName = $PLAIN
            $psi.Arguments = "`"$wav`" $($rate * 1000) $Passes"
            $psi.RedirectStandardOutput = $true
            $psi.UseShellExecute = $false
            $p = [System.Diagnostics.Process]::Start($psi)
            $null = $p.Handle
            try { $p.ProcessorAffinity = [IntPtr]16; $p.PriorityClass = 'High' } catch {}
            $tLine = $p.StandardOutput.ReadToEnd()
            $p.WaitForExit()
            $cpu = Get-Field $tLine 'cpu_ms'

            $rows += "$clip,$rate,$arm,$work,$cpu"
            Write-Host ("  {0,-20} {1,3}k {2,-7} work={3,-8} cpu_ms={4}" -f $clip, $rate, $arm, $work, $cpu)
        }
    }
}
$env:RUSTY_OPUS_FORCE_MODE = $null
"clip,rate_kbps,arm,work,cpu_ms" | Out-File $OUT -Encoding utf8
$rows | Out-File $OUT -Append -Encoding utf8
Write-Host "wrote $OUT ($($rows.Count) rows)"
