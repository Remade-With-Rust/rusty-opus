# Gate corpus — the Great Gate P0 content-class corpus

Ten clips, one per content class along which Opus tool payoffs vary
(`docs/great-gate.md` §2). All **48 kHz, 16-bit PCM, 12 s**. Regenerate with:

```sh
python tools/gen_gate_corpus.py          # deterministic, seed 0x0FF5
```

Real sources where the workspace has them; the classes no real corpus here
covered are synthesized **deterministically** so the corpus is reproducible
from the script rather than from a download.

| clip | ch | source | class it stresses | what it exists to catch |
|---|---|---|---|---|
| `speech_clean` | 1 | `fixtures/answer_16k.wav` (16 kHz speech, upsampled) | clean speech | SILK/hybrid mode routing; **band-limited content** (true bandwidth 8 kHz) |
| `speech_noisy` | 1 | above + pink noise @ ~12 dB SNR | noisy speech | VAD/DTX robustness; noise-vs-tonality classification |
| `mus_guitar` | 1 | rff corpus (Leyenda, PD) | tonal music, mono | tonality signals; CELT allocation |
| `mus_guitar_st` | 2 | rff corpus (stereo) | tonal music, stereo | stereo coupling, intensity, alloc trim |
| `mus_piano_st` | 2 | rff corpus (Open Goldberg, CC0) | tonal music, stereo, dense | the known stereo-music PEAQ gap |
| `percussive` | 1 | synth: castanet clicks + glockenspiel hits | transient/attack | block switching, TF resolution, weak-transient arms |
| `applause_st` | 2 | synth: ~45 claps/s, decorrelated L/R | noise-like | spreading/PVQ behaviour where tonality is meaningless |
| `wide_stereo` | 2 | piano mid + 15 ms Haas-decorrelated sides | wide stereo | M/S vs L/R, stereo width allocation |
| `silence_dtx` | 1 | speech in 1.8 s bursts, 1.2 s gaps, 20 ms fades | silence/activity | DTX, hangover, the abstention path |
| `mixed_speech_music` | 1 | 4 s speech → 4 s speech+guitar → 4 s guitar | **variable content** | the class that exposes every unfinished dispatch — mid-stream character change |

## Music-coverage classes (added 2026-08-07 after a coverage audit)

`tools/corpus_coverage.py` measures the signal properties that actually drive
Opus's decisions, and it showed the original music set was **solo acoustic
classical and nothing else**: bass energy topped out at 0.137, crest factors
never went below 14 dB, and the fastest real material was 7.2 onsets/s. The low
CELT bands, loudness-war masters and dense fast material were untested — which
is precisely where an unnoticed failure would live. These close that:

| clip | ch | what it covers | key measured property |
|---|---|---|---|
| `mus_vocal_st` | 2 | **real** PD vocal (Mozart aria) — formants + strong harmonics | crest 23.2 dB, centroid 1469 Hz |
| `mus_bass_edm` | 1 | electronic: sub-bass line + kick with real sub content | **bass frac 0.634** (was 0.137 max) |
| `mus_loud_master` | 1 | modern limited "loudness-war" master | **crest 8.0 dB** (was 14.4 min) |
| `mus_fast_dense` | 1 | 40 hits/s drums + tremolo — block-switch stressor | **19.8 onsets/s** (was 7.2 real max) |
| `mus_rock_dist_st` | 2 | clipped power chords + cymbals, decorrelated stereo | flatness 0.512, L/R −0.02, crest 11.4 dB |

They run ladders of **64 / 96 / 128 / 192 / 256 kb/s**, deliberately reaching
into the transparency region the original corpus never tested (it stopped at
160).

Four of the five are **synthetic** and labelled as such. They are correct for
stressing a *mechanism* — sub-bass allocation, rate control under constant
near-full-scale input, transient density, dense-harmonic spectra — and they are
not a substitute for real commercial masters when judging final perceptual
quality. The vocal is real.

## Rules that bind use of this corpus

- **Judge per clip, at ≥4 operating points**, with an EXTERNAL oracle (PEAQ
  ODG). Never a self-metric alone, never a single rate.
- **Compare at ACTUAL bitrate, never nominal.** libopus VBR overshoots its
  target by 15-20% on this corpus; ours lands on target. Comparing at the
  nominal rate hands libopus a free 15-20% and is the single easiest way to
  produce a wrong verdict here.
- **Worst class ≤ 0.** A change that averages positive while one class
  regresses is a dispatch signal, not a win.
- `mixed_speech_music` and `silence_dtx` are the classes most likely to expose
  a missing gate; a change that is neutral everywhere else and moves those two
  is still a result.

## The VoIP classes and the limits of PEAQ

`voip_*` are **operating-point** classes, not new content: the same
speech-family material scored under the `voip` application at 8-48 kb/s, i.e.
where SILK is the working arm. They exist because the corpus previously had no
coverage there at all, which is why the 2026-08-07 force-CELT ceiling
experiment (which removes SILK entirely) could not be turned into a shipping
decision.

**Read their ODG with care.** PEAQ is designed for wideband/fullband audio; on
narrowband speech at 8-16 kb/s it saturates and compresses. On the first
baseline both encoders' curves are nearly flat across the whole 8→32 kb/s
ladder (ours −3.86→−3.45, libopus −2.22→−2.15), which is far more likely to be
the metric running out of resolution than either codec failing to use the bits.
Use these classes as a **no-regression tripwire** for changes that touch mode
selection, and reach for a speech-domain metric (POLQA/PESQ) before drawing
any positive quality conclusion about the low-rate speech path.

## Known corpus limitations (state them, don't hide them)

- The music sources are the same short PD/CC0 clips the MP3/AAC campaigns used;
  they are real but few. A per-clip win here is weaker evidence than the same
  win across a broad library.
- `speech_clean` is **16 kHz-sourced**, upsampled to 48 kHz — deliberately, as
  the band-limited class — but that means it is not a valid test of true
  fullband speech. Add a natively-48 kHz speech clip before drawing fullband
  speech conclusions.
- The synthesized classes are physically plausible, not natural recordings.
  They are correct for stressing a *mechanism* (transient detector, DTX,
  stereo width); they are not a substitute for real applause/percussion when
  judging final perceptual quality.
