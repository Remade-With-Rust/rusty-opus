#!/usr/bin/env python3
"""Decoder conformance sweep: libopus-ENCODED streams, decoded by libopus and by
rusty-opus at every output rate x channel layout (the decoder axis the encoder
matrix never exercises: a 48 kHz stream played at 16 kHz, stereo->mono, ...).

Per cell:
  range  -- our decoder's per-packet final-range check (RANGECHK) against the
            encoder's ranges in the opus_demo stream: 0 mismatches required.
  diff   -- max |ours - libopus| in int16 samples, both decoding the same stream
            at the same output rate/channels. Exact (<=2) is the expectation for
            normal frames; float concealment near-ties are not in play (no loss).

Input: mixed-content corpus clips (speech/music switching drives libopus through
SILK/hybrid/CELT transitions, which is where redundancy and the transition
fades live). Streams are produced at every API rate.

Usage: OPUS_DEMO=<opus_demo.exe> python tools/coverage_decoder.py [--jobs N]
"""
import argparse, concurrent.futures as cf, json, os, subprocess, sys
import numpy as np
from scipy.io import wavfile
from scipy.signal import resample_poly

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
EX = os.path.join(ROOT, 'target', 'release', 'examples')
DEC = os.path.join(EX, 'decode_bit.exe' if os.name == 'nt' else 'decode_bit')
WORK = os.path.join(ROOT, 'target', 'coverage_decoder')
CORPUS = os.path.join(ROOT, 'fixtures', 'gate_corpus')
OPUS_DEMO = os.environ.get('OPUS_DEMO', '')

RATES = [8000, 12000, 16000, 24000, 48000]
CLIPS = ['mixed_speech_music.wav', 'voip_mixed.wav', 'mus_vocal_st.wav']
# (app, kbps, frame_ms) -- low/mid rates make libopus switch modes; 60 ms is
# multi-frame SILK / repacketized CELT, 10 ms is the CELT-only boundary.
PROFILES = [('voip', 12, '20'), ('audio', 24, '20'), ('audio', 32, '20'),
            ('audio', 64, '20'), ('voip', 16, '60'), ('audio', 48, '10')]


def run(cmd, env=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    p = subprocess.run(cmd, capture_output=True, text=True, env=e)
    return p.returncode, p.stdout + p.stderr


def make_input(clip, rate):
    sr, x = wavfile.read(os.path.join(CORPUS, clip))
    ch = 1 if x.ndim == 1 else x.shape[1]
    out = os.path.join(WORK, f'{clip}_{rate}.sw')
    if not os.path.exists(out):
        y = x.astype(np.float64)
        if rate != sr:
            y = resample_poly(y, rate, sr, axis=0)
        np.clip(np.round(y), -32768, 32767).astype(np.int16).tofile(out)
    return out, ch


def load(path, ch):
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        return None
    a = np.fromfile(path, dtype=np.int16).astype(np.int32)
    return a[: len(a) // ch * ch].reshape(-1, ch)


def stream(job):
    clip, rate, app, kbps, frame = job
    inp, ch = make_input(clip, rate)
    tag = f'{clip}_{rate}_{app}_{kbps}_{frame}'
    bit = os.path.join(WORK, tag + '.bit')
    rc, out = run([OPUS_DEMO, '-e', app, str(rate), str(ch), str(kbps * 1000),
                   '-framesize', frame, inp, bit])
    return job, ch, bit if rc == 0 else None


def cell(args):
    (clip, rate, app, kbps, frame), sch, bit, orate, och = args
    base = bit[:-4] + f'.d{orate}_{och}'
    lib, ours = base + '.lib.sw', base + '.ours.sw'
    res = dict(clip=clip, enc_rate=rate, enc_ch=sch, app=app, kbps=kbps, frame=frame,
               out_rate=orate, out_ch=och)
    rc, out = run([OPUS_DEMO, '-d', str(orate), str(och), bit, lib])
    res['lib_ok'] = rc == 0
    rc, out = run([DEC, str(orate), str(och), bit, ours], {'RANGECHK': '1'})
    res['range_ok'] = rc == 0 and 'RANGE: 0 mismatches' in out
    if not res['range_ok']:
        res['range_msg'] = [l for l in out.splitlines() if 'RANGE' in l][:2]
    a, b = load(lib, och), load(ours, och)
    if a is None or b is None:
        res['diff'] = -1
        return res
    n = min(len(a), len(b))
    res['len'] = (len(a), len(b))
    d = np.abs(a[:n] - b[:n]).max(axis=1) if n else np.array([0])
    res['diff'] = int(d.max())
    if res['diff'] > 2:
        res['first_bad_ms'] = round(float(np.nonzero(d > 2)[0][0]) * 1000 / orate, 2)
        res['bad_frac'] = round(float((d > 2).mean()), 5)
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--jobs', type=int, default=os.cpu_count())
    ap.add_argument('--out', default=os.path.join(WORK, 'decoder_matrix.json'))
    a = ap.parse_args()
    if not OPUS_DEMO:
        sys.exit('set OPUS_DEMO to a libopus opus_demo binary')
    os.makedirs(WORK, exist_ok=True)
    jobs = [(c, r, app, kb, f) for c in CLIPS for r in RATES for (app, kb, f) in PROFILES]
    with cf.ThreadPoolExecutor(a.jobs) as ex:
        streams = [s for s in ex.map(stream, jobs) if s[2]]
        cells = [(j, ch, bit, orate, och) for (j, ch, bit) in streams
                 for orate in RATES for och in (1, 2)]
        results = list(ex.map(cell, cells))
    json.dump(results, open(a.out, 'w'), indent=1)
    bad = [r for r in results if not r['range_ok'] or r['diff'] > 2 or r['diff'] < 0]
    print(f'{len(streams)}/{len(jobs)} streams, {len(results)} decode cells, '
          f'{len(results) - len(bad)} exact, {len(bad)} off')
    by = {}
    for r in bad:
        k = (r['enc_rate'], r['out_rate'], r['enc_ch'], r['out_ch'], r['frame'])
        by.setdefault(k, []).append(r)
    for k, rs in sorted(by.items()):
        worst = max(rs, key=lambda r: r['diff'])
        print(f'  enc {k[0]}/{k[2]}ch -> out {k[1]}/{k[3]}ch {k[4]}ms: {len(rs)} cells, '
              f'range_bad={sum(not r["range_ok"] for r in rs)} worst diff={worst["diff"]} '
              f'({worst["clip"]} {worst["app"]}{worst["kbps"]} first_bad={worst.get("first_bad_ms")}ms '
              f'frac={worst.get("bad_frac")})')


if __name__ == '__main__':
    main()
