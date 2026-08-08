# Three-way ENCODE speed benchmark: rusty-opus vs libopus vs ffmpeg-native.
#
# For numbers that go in a README, the discipline is not optional
# (codec-measurement):
#   par.5  Every arm is measured the SAME way - as a process, on a 60 s and a
#          30 s clip, reporting (t60 - t30). That slope removes process
#          startup, file I/O and setup for ALL arms, so no arm is credited or
#          charged for harness geometry.
#   par.1  Pinned to one core at High priority; CPU time, not wall.
#   par.3  Arms ABBA-alternated each rep, plus a NULL arm (the same binary
#          against itself) to establish the resolution floor.
#   par.16 A CROSS-IMPLEMENTATION ratio needs N >= 31 and the median must be
#          STABLE as N grows - so we report the ratio at N=15/31/41 and refuse
#          to headline one that is still trending.
#
#   powershell tools\bench_encode_3way.ps1 -Reps 41
# Clip lengths matter more than they look: Windows reports TotalProcessorTime in
# ~15.6 ms quanta. At 60/30 s the slope is 78-470 ms, i.e. only 5-30 quanta, and
# the null arm (identical code, two names) read 9.1% apart - so nothing below
# ~10% was resolvable and a "1.20x" was literally one quantum. At 300/150 s the
# slope is ~10x larger and the quantum is under 1% of it.
param([int]$Reps = 41, [int]$Long = 300, [int]$Short = 150)

$ROOT = Split-Path -Parent $PSScriptRoot
$T = Join-Path $ROOT "target"
$C = Join-Path $ROOT "fixtures\gate_corpus"
$B = Join-Path $T "bench3"
New-Item -ItemType Directory -Force $B | Out-Null
$FF = (Get-Command ffmpeg).Source
$OURS = Join-Path $T "release\examples\encode_ogg.exe"

# Long and short sources, looped from the corpus so content is identical.
function Ensure($name, $src, $secs) {
    $p = Join-Path $B "$name.wav"
    if (-not (Test-Path $p)) {
        & $FF -hide_banner -loglevel error -y -stream_loop 200 -i $src -t $secs -ar 48000 -sample_fmt s16 $p
    }
    return $p
}
function EnsureRate($name, $src, $secs, $rate) {
    $p = Join-Path $B "$name.wav"
    if (-not (Test-Path $p)) {
        & $FF -hide_banner -loglevel error -y -stream_loop 400 -i $src -t $secs -ar $rate -sample_fmt s16 $p
    }
    return $p
}
$speech60 = Ensure "speech_long_$Long"  (Join-Path $C "speech_clean.wav")   $Long
$speech30 = Ensure "speech_short_$Short" (Join-Path $C "speech_clean.wav")  $Short
$music60  = Ensure "music_long_$Long"   (Join-Path $C "mus_guitar_st.wav")  $Long
$music30  = Ensure "music_short_$Short" (Join-Path $C "mus_guitar_st.wav")  $Short
# SILK path: kept at 16 kHz on purpose. Verified by TOC dump that BOTH encoders
# emit silk/WB 100% here, whereas every 48 kHz config - including
# `-application voip` - ends up CELT on this material because the classifier
# reads looped speech as music. Without this arm the benchmark would silently
# be CELT-only and would flatter us, since libopus's hand-written NSQ assembly
# is its strongest ground.
$silk60 = EnsureRate "silk_long_$Long"   (Join-Path $ROOT "fixtures\answer_16k.wav") $Long  16000
$silk30 = EnsureRate "silk_short_$Short" (Join-Path $ROOT "fixtures\answer_16k.wav") $Short 16000
$span = $Long - $Short

function Invoke-Pinned($exe, $argl) {
    $p = Start-Process -FilePath $exe -ArgumentList $argl -PassThru -WindowStyle Hidden
    $null = $p.Handle
    try { $p.ProcessorAffinity = [IntPtr]16; $p.PriorityClass = 'High' } catch {}
    $p.WaitForExit()
    return $p.TotalProcessorTime.TotalMilliseconds
}

function Arms($tag, $wav60, $wav30, $kbps, $app = 'audio') {
    @(
        @{n="ours_$tag";  e=$OURS; a60="`"$wav60`" `"$B\o60_$tag.opus`" $($kbps*1000) $app"; a30="`"$wav30`" `"$B\o30_$tag.opus`" $($kbps*1000) $app"},
        @{n="lib_$tag";   e=$FF;   a60="-hide_banner -loglevel error -y -i `"$wav60`" -c:a libopus -b:a ${kbps}k -application $app `"$B\l60_$tag.opus`""; a30="-hide_banner -loglevel error -y -i `"$wav30`" -c:a libopus -b:a ${kbps}k -application $app `"$B\l30_$tag.opus`""},
        # ffmpeg's native encoder has no -application switch (CELT-only).
        @{n="nat_$tag";   e=$FF;   a60="-hide_banner -loglevel error -y -i `"$wav60`" -strict -2 -c:a opus -b:a ${kbps}k `"$B\n60_$tag.opus`""; a30="-hide_banner -loglevel error -y -i `"$wav30`" -strict -2 -c:a opus -b:a ${kbps}k `"$B\n30_$tag.opus`""},
        # NULL arm: ours measured twice under different names. Any apparent
        # difference between these two IS the harness's resolution floor.
        @{n="null_$tag";  e=$OURS; a60="`"$wav60`" `"$B\z60_$tag.opus`" $($kbps*1000) $app"; a30="`"$wav30`" `"$B\z30_$tag.opus`" $($kbps*1000) $app"}
    )
}

# Three configurations on purpose. `audio` on speech routes to CELT after the
# analysis warm-up fix, so speech_audio is a CELT-vs-CELT race; the SILK path
# only gets exercised by the `voip` arm, and that is where libopus's hand-written
# NSQ assembly is strongest. Reporting only the first would flatter us.
$all = (Arms "speechCelt" $speech60 $speech30  32 'audio') +
       (Arms "speechSilk" $silk60   $silk30    16 'voip')  +
       (Arms "music"      $music60  $music30  128 'audio')
$slopes = @{}
foreach ($a in $all) { $slopes[$a.n] = @() }

Write-Host "method: pinned core, High priority, CPU time, ABBA, slope = t(60s) - t(30s), reps=$Reps"
foreach ($rep in 1..$Reps) {
    $order = if ($rep % 2 -eq 0) { $all } else { $all[($all.Count - 1)..0] }
    foreach ($a in $order) {
        $t60 = Invoke-Pinned $a.e $a.a60
        $t30 = Invoke-Pinned $a.e $a.a30
        $slopes[$a.n] += ($t60 - $t30)
    }
    if ($rep -in 15, 31, $Reps) {
        Write-Host "`n--- after $rep reps (median slope; xRT = span/slope) ---"
        foreach ($a in $all) {
            $s = $slopes[$a.n] | Sort-Object
            $med = $s[[int]($s.Count / 2)]
            if ($med -gt 0) {
                "{0,-14} slope {1,9:N1} ms for {2}s audio   {3,7:N0}x realtime   ({4:N1} timer quanta)" -f `
                    $a.n, $med, $span, ($span * 1000 / $med), ($med / 15.625)
            } else {
                "{0,-14} slope {1,9:N1} ms   (below timer resolution)" -f $a.n, $med
            }
        }
    }
}

Write-Host "`n=== ratios (ours / reference), watch for a TREND across N ==="
foreach ($tag in 'speechCelt', 'speechSilk', 'music') {
    $o = ($slopes["ours_$tag"] | Sort-Object)[[int]($Reps / 2)]
    $l = ($slopes["lib_$tag"]  | Sort-Object)[[int]($Reps / 2)]
    $n = ($slopes["nat_$tag"]  | Sort-Object)[[int]($Reps / 2)]
    $z = ($slopes["null_$tag"] | Sort-Object)[[int]($Reps / 2)]
    "{0}: ours {1:N1} ms | libopus {2:N1} ({3:N2}x) | ffmpeg-native {4:N1} ({5:N2}x) | null-arm floor {6:N1} ms ({7:P1} of ours)" -f `
        $tag, $o, $l, ($l / $o), $n, ($n / $o), $z, ([math]::Abs($z - $o) / $o)
}

