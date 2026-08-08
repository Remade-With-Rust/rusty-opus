#!/usr/bin/env python3
"""Objectively characterise what the corpus actually covers.

A per-class ODG table only protects you on the classes you HAVE. This measures
the signal properties that drive Opus's decisions, so coverage gaps are visible
as numbers rather than as someone's opinion about genre:

  onsets/s     transient density -> block switching, pre-echo, TF resolution
  crest dB     peak-to-RMS -> how compressed/"loud-war" the master is
  centroid Hz  spectral balance -> bandwidth + allocation
  HF frac      energy above 10 kHz -> the bandwidth detector's own input
  bass frac    energy below 200 Hz -> sub-bass handling, where Opus is thinnest
  flatness     tonal (low) vs noise-like (high) -> tonality/PVQ behaviour
  L/R corr     stereo width -> M/S vs L/R, intensity stereo

  python tools/corpus_coverage.py [dir]
"""
import os, sys, glob
import numpy as np
from scipy.io import wavfile

D = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'fixtures', 'gate_corpus')

def analyse(path):
    sr, d = wavfile.read(path)
    d = d.astype(np.float64) / 32768.0
    stereo = d.ndim > 1
    corr = float(np.corrcoef(d[:, 0], d[:, 1])[0, 1]) if stereo and d.shape[0] > 1 else 1.0
    x = d.mean(axis=1) if stereo else d
    rms = float(np.sqrt(np.mean(x ** 2))) + 1e-12
    crest = 20 * np.log10(float(np.max(np.abs(x)) + 1e-12) / rms)

    n = 1024
    hop = 512
    frames = 1 + (len(x) - n) // hop
    win = np.hanning(n)
    S = np.abs(np.fft.rfft(np.stack([x[i*hop:i*hop+n] * win for i in range(frames)]), axis=1))
    freqs = np.fft.rfftfreq(n, 1.0 / sr)
    e = S.sum(axis=1) + 1e-12
    centroid = float(np.mean((S * freqs).sum(axis=1) / e))
    hf = float(np.mean(S[:, freqs > 10000].sum(axis=1) / e))
    bass = float(np.mean(S[:, freqs < 200].sum(axis=1) / e))
    gm = np.exp(np.mean(np.log(S + 1e-12), axis=1))
    am = np.mean(S, axis=1) + 1e-12
    flat = float(np.mean(gm / am))
    # Spectral flux onsets: half-wave-rectified increase, peaks over an
    # adaptive median threshold. Tempo-agnostic, so it works on any genre.
    flux = np.maximum(0, np.diff(S, axis=0)).sum(axis=1)
    if len(flux) > 20:
        thr = np.median(flux) + 1.5 * np.std(flux)
        peaks = (flux > thr) & (flux > np.roll(flux, 1)) & (flux > np.roll(flux, -1))
        onsets = float(peaks.sum() / (len(x) / sr))
    else:
        onsets = 0.0
    return dict(sr=sr, ch=(2 if stereo else 1), onsets=onsets, crest=crest,
                centroid=centroid, hf=hf, bass=bass, flat=flat, corr=corr)

rows = []
for p in sorted(glob.glob(os.path.join(D, '*.wav'))):
    try:
        rows.append((os.path.basename(p)[:-4], analyse(p)))
    except Exception as ex:
        print(f'{os.path.basename(p)}: {ex}')

print(f'{"clip":22s} {"ch":>2s} {"onset/s":>8s} {"crest":>7s} {"centr":>7s} '
      f'{"HF>10k":>7s} {"bass":>6s} {"flat":>6s} {"L/R":>6s}')
for name, a in rows:
    print(f'{name:22s} {a["ch"]:>2d} {a["onsets"]:>8.1f} {a["crest"]:>6.1f}dB '
          f'{a["centroid"]:>7.0f} {a["hf"]:>7.3f} {a["bass"]:>6.3f} {a["flat"]:>6.3f} {a["corr"]:>6.2f}')

mus = [(n, a) for n, a in rows if n.startswith('mus_') or n in ('percussive', 'applause_st', 'wide_stereo')]
if mus:
    print('\n--- music-family coverage ---')
    for k, lab in (('onsets', 'transient density (onsets/s)'), ('crest', 'crest factor dB'),
                   ('centroid', 'spectral centroid Hz'), ('bass', 'bass energy frac')):
        v = [a[k] for _, a in mus]
        print(f'{lab:32s} min {min(v):8.2f}   max {max(v):8.2f}')
