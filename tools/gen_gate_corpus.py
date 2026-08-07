#!/usr/bin/env python3
"""Great Gate P0: build the audio content-class corpus (great-gate.md §2).

Ten clips, one per content class the Opus tool payoffs vary along; every clip
48 kHz s16, ~12 s. Real sources where we have them (speech fixture, the rff
music corpus); the corpus-gap classes (percussive/transient, applause,
noisy speech, wide-stereo, silence/DTX, mixed speech+music) are synthesized
DETERMINISTICALLY (fixed seed) so the corpus is reproducible from this script.

Usage:  python tools/gen_gate_corpus.py [outdir]     (default fixtures/gate_corpus)
Needs:  ffmpeg on PATH; numpy+scipy; the remade_ffmpeg_rs checkout next door
        (for the music corpus WAVs).
"""
import os, subprocess, sys
import numpy as np
from scipy.io import wavfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
RFF_CORPUS = os.path.normpath(os.path.join(ROOT, '..', 'remade_ffmpeg_rs', 'corpus'))
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, 'fixtures', 'gate_corpus')
SR = 48000
DUR = 12.0
rng = np.random.default_rng(0x0FF5)  # deterministic corpus

os.makedirs(OUT, exist_ok=True)

def ff(*args):
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', *args], check=True)

def load(path):
    sr, d = wavfile.read(path)
    assert sr == SR, f'{path}: {sr} != {SR}'
    return d.astype(np.float64) / 32768.0

def save(name, x):
    """x: float in [-1,1], shape (n,) mono or (n,2) stereo."""
    x = np.clip(x, -1.0, 1.0)
    wavfile.write(os.path.join(OUT, name), SR, (x * 32767.0).astype(np.int16))
    print(f'  {name}: {x.shape[0]/SR:.1f}s {"stereo" if x.ndim>1 else "mono"}')

n = int(DUR * SR)

# --- real-source classes (resample/loop via ffmpeg into temp, then trim) ---
tmp = os.path.join(OUT, '_tmp.wav')
def real(src, name, loops=0, stereo=False):
    args = []
    if loops: args += ['-stream_loop', str(loops)]
    args += ['-i', src, '-ar', str(SR), '-sample_fmt', 's16', '-t', str(DUR), tmp]
    ff(*args)
    x = load(tmp)
    save(name, x)
    return x

print('real-source classes:')
speech = real(os.path.join(ROOT, 'fixtures', 'answer_16k.wav'), 'speech_clean.wav')
guitar = real(os.path.join(RFF_CORPUS, 'corp_long_mus_guitar.wav'), 'mus_guitar.wav')
real(os.path.join(RFF_CORPUS, 'corp_st_mus_piano.wav'), 'mus_piano_st.wav', loops=2)
real(os.path.join(RFF_CORPUS, 'corp_st_mus_guitar.wav'), 'mus_guitar_st.wav', loops=2)
os.remove(tmp)
if speech.ndim > 1: speech = speech[:, 0]
if guitar.ndim > 1: guitar = guitar[:, 0]
speech = np.resize(speech, n); guitar = np.resize(guitar, n)

print('synthesized gap classes:')

# --- noisy speech: clean speech + pink noise at ~12 dB SNR ---
white = rng.standard_normal(n + 1)
pink = np.cumsum(white)                      # 1/f-ish via integration
pink -= np.linspace(0, pink[-1], n + 1)      # detrend so it stays bounded
pink = np.diff(np.cumsum(pink) / 50.0)       # smooth
pink /= np.max(np.abs(pink))
sp_pow = np.mean(speech ** 2)
noise = pink * np.sqrt(sp_pow / 10 ** (12 / 10) / np.mean(pink ** 2))
save('speech_noisy.wav', 0.9 * (speech + noise) / np.max(np.abs(speech + noise)) )

# --- percussive/transient: castanet-like noise bursts + glockenspiel hits ---
x = np.zeros(n)
t0 = 0.1
while t0 < DUR - 0.1:
    i = int(t0 * SR)
    blen = int(0.008 * SR)                                   # 8 ms click
    burst = rng.standard_normal(blen) * np.exp(-np.arange(blen) / (0.002 * SR))
    x[i:i + blen] += 0.8 * burst
    if rng.random() < 0.35:                                  # occasional glock hit
        f = rng.choice([1568.0, 2093.0, 2637.0, 3136.0])
        glen = int(0.4 * SR)
        tt = np.arange(glen) / SR
        hit = (np.sin(2 * np.pi * f * tt) + 0.4 * np.sin(2 * np.pi * 2.71 * f * tt)) \
              * np.exp(-tt / 0.08)
        j = min(i, n - glen)
        x[j:j + glen] += 0.35 * hit
    t0 += float(rng.uniform(0.08, 0.25))
save('percussive.wav', 0.95 * x / np.max(np.abs(x)))

# --- applause: dense overlapping decaying claps, slight stereo decorrelation ---
def clap_track():
    y = np.zeros(n)
    for _ in range(int(DUR * 45)):                           # ~45 claps/s
        i = int(rng.uniform(0, DUR - 0.05) * SR)
        clen = int(0.03 * SR)
        y[i:i + clen] += rng.standard_normal(clen) * np.exp(-np.arange(clen) / (0.004 * SR))
    return y
L, R = clap_track(), clap_track()
st = np.stack([L, R], axis=1)
save('applause_st.wav', 0.9 * st / np.max(np.abs(st)))

# --- wide stereo: piano mid + strongly decorrelated sides (Haas + inversion mix) ---
piano = load(os.path.join(OUT, 'mus_piano_st.wav'))
mid = piano.mean(axis=1)
d = int(0.015 * SR)                                          # 15 ms Haas delay
side = np.roll(mid, d)
Lw = mid + 0.7 * side
Rw = mid - 0.7 * side
wide = np.stack([Lw, Rw], axis=1)
save('wide_stereo.wav', 0.9 * wide / np.max(np.abs(wide)))

# --- silence/DTX-shaped: speech in 1.8s bursts with 1.2s silent gaps, 20ms fades ---
x = speech.copy()
t = 0.0
fade = int(0.02 * SR)
env = np.ones(n)
while t < DUR:
    a, b = int((t + 1.8) * SR), int((t + 3.0) * SR)
    if a < n:
        b = min(b, n)
        env[a:b] = 0.0
        if a - fade >= 0: env[a - fade:a] = np.linspace(1, 0, fade)
        if b + fade <= n: env[b:b + fade] = np.linspace(0, 1, fade)
    t += 3.0
save('silence_dtx.wav', x * env)

# --- mixed speech+music: 4s speech -> 4s speech-over-guitar -> 4s guitar ---
third = n // 3
x = np.zeros(n)
x[:third] = speech[:third]
x[third:2 * third] = 0.7 * speech[third:2 * third] + 0.5 * guitar[third:2 * third]
x[2 * third:] = guitar[2 * third:3 * third - (3 * third - n)] if 3 * third > n else guitar[2 * third:3 * third]
x[2 * third:] = guitar[2 * third:n]
save('mixed_speech_music.wav', 0.95 * x / np.max(np.abs(x)))

# --- VoIP / low-rate operating-point classes -------------------------------
# These are NOT new content: they are the same speech-family material scored
# under the `voip` application at 8-32 kb/s, i.e. exactly where SILK is the
# working arm. The corpus had no such class, which is why the 2026-08-07
# force-CELT experiment could not be trusted as a shipping decision (it removes
# SILK, and nothing in the corpus was measuring SILK). Named `voip_*` so the
# ladder can route them to the voip application.
print('voip / low-rate operating-point classes:')
save('voip_speech.wav', speech)
noisy = load(os.path.join(OUT, 'speech_noisy.wav'))
save('voip_speech_noisy.wav', noisy)
save('voip_mixed.wav', load(os.path.join(OUT, 'mixed_speech_music.wav')))

print(f'corpus complete in {OUT}')
