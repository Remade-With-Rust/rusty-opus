#!/usr/bin/env python3
"""Great Gate P0/P4: the per-clip ladder harness — ODG at >=4 operating points
per corpus clip, external PEAQ oracle (never a self-metric), CSV out.

The corpus law (great-gate.md §2) and the regression harness (P4) both read
this. Arms:
  ours  — rusty-opus encode+decode via the roundtrip example (full shipped stack)
  lib   — ffmpeg -c:a libopus encode, ffmpeg decode (the reference anchor)

Usage:
  python tools/gate_ladder.py [--arms ours,lib] [--clips substr,substr]
        [--rates 16,24,32] [--out ladder.csv] [--tag label]
Defaults: all corpus clips, per-class rate ladders, arms=ours.

Freshness law: the roundtrip exe's mtime is printed and the harness REFUSES an
exe older than any file in src/ (stale-binary sabotage, codec-measurement §10).
PEAQ comes from the remade_ffmpeg_rs checkout (RFF_ROOT env overrides).
"""
import argparse, contextlib, io, os, subprocess, sys, warnings
import numpy as np
warnings.filterwarnings('ignore')
from scipy.io import wavfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
RFF = os.environ.get('RFF_ROOT', os.path.normpath(os.path.join(ROOT, '..', 'remade_ffmpeg_rs')))
sys.path.insert(0, os.path.join(RFF, 'PEAQ_python'))
import numpy_PEAQ

CORPUS = os.path.join(ROOT, 'fixtures', 'gate_corpus')
RT = os.path.join(ROOT, 'target', 'release', 'examples',
                  'roundtrip.exe' if os.name == 'nt' else 'roundtrip')
# Pin to an explicit binary (e.g. a copy taken before an unrelated edit) so two
# concurrent ladder runs are guaranteed to be the SAME encoder. Setting this
# bypasses the staleness check — you are asserting you know which binary it is.
RT_PINNED = os.environ.get('RUSTY_OPUS_ROUNDTRIP')
if RT_PINNED:
    RT = RT_PINNED

# per-class rate ladders (kbps) — speech-band classes low, music classes higher
LADDERS = {
    'speech_clean':       [16, 24, 32, 48, 64],
    'speech_noisy':       [16, 24, 32, 48, 64],
    'silence_dtx':        [16, 24, 32, 48, 64],
    'mixed_speech_music': [24, 32, 48, 64, 96],
    'mus_guitar':         [32, 48, 64, 96, 128],
    'percussive':         [32, 48, 64, 96, 128],
    'mus_piano_st':       [48, 64, 96, 128, 160],
    'mus_guitar_st':      [48, 64, 96, 128, 160],
    'applause_st':        [48, 64, 96, 128, 160],
    'wide_stereo':        [48, 64, 96, 128, 160],
    # VoIP / low-rate operating points — where SILK is the working arm.
    'voip_speech':        [8, 12, 16, 24, 32],
    'voip_speech_noisy':  [8, 12, 16, 24, 32],
    'voip_mixed':         [12, 16, 24, 32, 48],
    # Music-coverage classes. These deliberately run INTO the 192-256 kb/s
    # transparency region: that is where Opus music encoding actually lives and
    # the original corpus stopped at 160, so the top of the range was untested.
    'mus_vocal_st':       [64, 96, 128, 192, 256],
    'mus_bass_edm':       [64, 96, 128, 192, 256],
    'mus_loud_master':    [64, 96, 128, 192, 256],
    'mus_fast_dense':     [64, 96, 128, 192, 256],
    'mus_rock_dist_st':   [64, 96, 128, 192, 256],
}

# Opus application per clip; `voip` biases toward SILK and is the mode the
# low-rate classes exist to exercise. Everything else uses `audio`.
def app_for(clip):
    return 'voip' if clip.startswith('voip_') else 'audio'

def check_fresh():
    if not os.path.exists(RT):
        sys.exit(f'build first: cargo build --release --example roundtrip ({RT} missing)')
    if RT_PINNED:
        print(f'PINNED binary (staleness check bypassed): {RT} '
              f'mtime {os.path.getmtime(RT):.0f}')
        return
    exe_m = os.path.getmtime(RT)
    newest = max(os.path.getmtime(os.path.join(dp, f))
                 for dp, _, fs in os.walk(os.path.join(ROOT, 'src')) for f in fs)
    print(f'roundtrip.exe mtime {exe_m:.0f}, newest src {newest:.0f}')
    if newest > exe_m:
        sys.exit('STALE BINARY: src/ is newer than roundtrip.exe — rebuild (codec-measurement §10)')

def load(p):
    sr, d = wavfile.read(p)
    if d.ndim > 1: d = d[:, 0]
    d = d.astype(np.float64)
    if np.max(np.abs(d)) <= 1.5: d *= 32768.0
    if np.max(np.abs(d)) == 32768.0:  # PEAQ_python full-scale guard
        d *= (32768.0 - 0.01) / 32768.0
    return d, sr

def best_delay(ref, test, maxd=3000):
    n = min(len(ref), len(test))
    if n < maxd + 8000: return 0
    w = min(n - maxd, 80000); start = (n - maxd - w) // 2
    r = ref[start:start+w:32]
    best = (1e30, 0)
    for d in range(maxd):
        e = test[start+d:start+d+w:32] - r
        err = float(e @ e)
        if err < best[0]: best = (err, d)
    return best[1]

def peaq(refp, testp):
    ref, sr = load(refp); test, _ = load(testp)
    d = best_delay(ref, test); test = test[d:]
    m = min(len(ref), len(test)); ref, test = ref[:m], test[:m]
    p = numpy_PEAQ.PEAQ(32768, Fs=sr)
    with contextlib.redirect_stdout(io.StringIO()):
        p.process(ref, test)
        r = p.avg_get()
    return float(r['ODG'] if isinstance(r, dict) and 'ODG' in r else r)

def encode_ours(inp, outp, kbps, clip='', app='audio'):
    env = os.environ.copy()
    if env.get('RUSTY_OPUS_GATE_HARVEST'):
        env['RUSTY_OPUS_GATE_CLIP'] = clip
    r = subprocess.run([RT, inp, outp, str(kbps * 1000), app],
                       capture_output=True, text=True, check=True, env=env)
    for line in r.stderr.splitlines():
        if 'encoded' in line:
            b = int(line.split('encoded')[1].split('bytes')[0])
            s = float(line.split('over')[1].split('s')[0])
            return b * 8.0 / s / 1000.0
    return -1.0

def encode_nat(inp, outp, kbps, work):
    """FFmpeg's OWN native Opus encoder (`-c:a opus`), not libopus. It is marked
    experimental (`-strict -2`) and has no `-application` switch."""
    opus = os.path.join(work, '_nat.opus')
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', inp,
                    '-strict', '-2', '-c:a', 'opus', '-b:a', f'{kbps}k', opus], check=True)
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', opus,
                    '-ar', '48000', outp], check=True)
    sz = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'a',
                         '-show_entries', 'packet=size', '-of', 'csv=p=0', opus],
                        capture_output=True, text=True).stdout
    b = sum(int(x.strip().rstrip(',')) for x in sz.split() if x.strip().rstrip(','))
    return b * 8.0 / 12.0 / 1000.0

def encode_lib(inp, outp, kbps, work, app='audio'):
    opus = os.path.join(work, '_lib.opus')
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', inp,
                    '-c:a', 'libopus', '-b:a', f'{kbps}k', '-application', app, opus], check=True)
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', opus,
                    '-ar', '48000', outp], check=True)
    sz = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'a',
                         '-show_entries', 'packet=size', '-of', 'csv=p=0', opus],
                        capture_output=True, text=True).stdout
    bytes_ = sum(int(x.strip().rstrip(',')) for x in sz.split() if x.strip().rstrip(','))
    return bytes_ * 8.0 / 12.0 / 1000.0

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--arms', default='ours')
    ap.add_argument('--clips', default='')
    ap.add_argument('--rates', default='')
    ap.add_argument('--out', default=os.path.join(ROOT, 'ladder.csv'))
    ap.add_argument('--tag', default='')
    # Scratch dir for intermediates. Give concurrent ladder runs DIFFERENT dirs:
    # decoded WAVs are named arm_clip_rate.wav and libopus staging is a fixed
    # _lib.opus, so two runs sharing a dir would overwrite each other's files.
    ap.add_argument('--workdir', default=os.path.join(ROOT, 'target', 'gate_ladder_tmp'))
    a = ap.parse_args()
    arms = a.arms.split(',')
    if 'ours' in arms: check_fresh()
    clips = sorted(LADDERS)
    if a.clips:
        subs = a.clips.split(',')
        clips = [c for c in clips if any(s in c for s in subs)]
    work = a.workdir
    os.makedirs(work, exist_ok=True)
    new = not os.path.exists(a.out)
    out = open(a.out, 'a')
    if new: out.write('clip,rate_kbps,arm,actual_kbps,odg,tag\n')
    for clip in clips:
        refp = os.path.join(CORPUS, clip + '.wav')
        rates = [int(r) for r in a.rates.split(',')] if a.rates else LADDERS[clip]
        for kbps in rates:
            for arm in arms:
                dec = os.path.join(work, f'{arm}_{clip}_{kbps}.wav')
                app = app_for(clip)
                if arm == 'ours':
                    actual = encode_ours(refp, dec, kbps, clip=clip, app=app)
                elif arm == 'lib':
                    actual = encode_lib(refp, dec, kbps, work, app=app)
                elif arm == 'nat':
                    actual = encode_nat(refp, dec, kbps, work)
                else:
                    sys.exit(f'unknown arm {arm}')
                odg = peaq(refp, dec)
                out.write(f'{clip},{kbps},{arm},{actual:.1f},{odg:.4f},{a.tag}\n')
                out.flush()
                print(f'{clip}@{kbps}k {arm}: {actual:.1f} kbps ODG {odg:+.3f}', flush=True)
    out.close()

if __name__ == '__main__':
    main()
