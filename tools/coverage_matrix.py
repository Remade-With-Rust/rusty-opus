#!/usr/bin/env python3
"""Opus coverage matrix: every configuration libopus supports, encoded by US and
checked against libopus at every step.

Per cell (API rate x channels x application x frame size x bandwidth x bitrate):

  1. support   -- libopus `opus_demo -e` encodes the cell; do we?  (a cell libopus
                  codes and we refuse is a coverage gap, not a configuration error)
  2. range     -- libopus `opus_demo -d` decodes OUR packets with the per-packet
                  final-range check: any encoder/decoder range-coder divergence fails
  3. decoder   -- OUR decoder vs libopus's decoder on the same (our) stream,
                  max |diff| in s16 LSB
  4. fidelity  -- libopus-decoded output of OUR stream vs the input, delay-aligned
                  SNR, next to the same number for LIBOPUS's own stream of the cell
                  (catches "decodes cleanly to garbage", which 2 and 3 cannot)
  5. routing   -- TOC census: modal mode/bandwidth/frame of our packets vs libopus's

Deterministic: no timing anywhere, so it runs on a busy box. Every number is a
count, a hash-free sample comparison or an exit status.

  OPUS_DEMO=<path to opus_demo.exe> python3 tools/coverage_matrix.py [--quick] [--jobs N]
"""
import argparse, collections, concurrent.futures as cf, itertools, json, os, struct
import subprocess, sys
import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EX = os.path.join(ROOT, 'target', 'release', 'examples')
# ENC_BIN / DEC_BIN pin explicit binaries (e.g. copies taken before further
# edits) so a long run is guaranteed to measure ONE build.
ENC = os.environ.get('ENC_BIN', os.path.join(EX, 'encode_bit.exe'))
DEC = os.environ.get('DEC_BIN', os.path.join(EX, 'decode_bit.exe'))
OPUS_DEMO = os.environ.get('OPUS_DEMO')
WORK = os.path.join(ROOT, 'target', 'coverage_matrix')
CORPUS = os.path.join(ROOT, 'fixtures', 'gate_corpus')
SRC = {1: 'mixed_speech_music.wav', 2: 'mus_vocal_st.wav'}
CLIP_S = 3.0

RATES = [8000, 12000, 16000, 24000, 48000]
APPS = ['voip', 'audio', 'lowdelay']
FRAMES = ['2.5', '5', '10', '20', '40', '60', '80', '100', '120']
BWS = ['auto', 'nb', 'mb', 'wb', 'swb', 'fb']
LIB_APP = {'voip': 'voip', 'audio': 'audio', 'lowdelay': 'restricted-lowdelay'}


def prep_inputs():
    os.makedirs(WORK, exist_ok=True)
    for ch, name in SRC.items():
        for r in RATES:
            out = os.path.join(WORK, f'in_{r}_{ch}.sw')
            if os.path.exists(out):
                continue
            subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
                            '-i', os.path.join(CORPUS, name), '-t', str(CLIP_S),
                            '-ac', str(ch), '-ar', str(r), '-f', 's16le', out], check=True)


def read_bit(path):
    """opus_demo framing -> list of payloads."""
    data = open(path, 'rb').read()
    pos, pk = 0, []
    while pos + 8 <= len(data):
        n = struct.unpack('>I', data[pos:pos + 4])[0]
        pos += 8
        pk.append(data[pos:pos + n])
        pos += n
    return pk


def describe(p):
    if not p:
        return ('empty', '-', 0.0, 0)
    c = p[0] >> 3
    if c < 12:
        mode, bw, ms = 'silk', ['nb', 'mb', 'wb'][c // 4], [10, 20, 40, 60][c % 4]
    elif c < 16:
        mode, bw, ms = 'hybrid', ['swb', 'fb'][(c - 12) // 2], [10, 20][c % 2]
    else:
        mode, bw, ms = 'celt', ['nb', 'wb', 'swb', 'fb'][(c - 16) // 4], [2.5, 5, 10, 20][c % 4]
    code = p[0] & 3
    nf = 1 if code == 0 else 2 if code in (1, 2) else (p[1] & 0x3F if len(p) > 1 else 0)
    return (mode, bw, ms * nf, nf)


def census(packets):
    h = collections.Counter()
    for p in packets:
        if len(p) <= 2:  # DTX / empty
            continue
        m, b, ms, _ = describe(p)
        h[(m, b, ms)] += 1
    return h


def load(path, ch):
    if not os.path.exists(path):
        return None
    x = np.fromfile(path, dtype='<i2').astype(np.float64)
    return x.reshape(-1, ch) if ch > 1 else x.reshape(-1, 1)


def best_lag(a, b, max_lag):
    """Exact integer lag L in [0, max_lag] maximising sum a[t] * b[t + L]."""
    n = len(a) + len(b)
    nfft = 1 << (n - 1).bit_length()
    xc = np.fft.irfft(np.fft.rfft(b, nfft) * np.conj(np.fft.rfft(a, nfft)), nfft)
    return int(np.argmax(xc[:max_lag + 1]))


def aligned_snr(ref, out, rate):
    """SNR after exact (sample-accurate) delay alignment within 30 ms, per-channel
    error summed over channels; the first 100 ms is skipped as warm-up."""
    if out is None or len(out) == 0:
        return float('nan')
    lag = best_lag(ref.mean(axis=1), out.mean(axis=1), int(rate * 0.030))
    skip = rate // 10
    n = min(len(ref), len(out) - lag) - skip
    if n <= rate // 10:
        return float('nan')
    a = ref[skip:skip + n]
    b = out[skip + lag:skip + lag + n]
    sig = err = 0.0
    for c in range(a.shape[1]):
        g = np.dot(a[:, c], b[:, c]) / (np.dot(b[:, c], b[:, c]) + 1e-9)
        sig += np.dot(a[:, c], a[:, c])
        err += np.dot(a[:, c] - g * b[:, c], a[:, c] - g * b[:, c])
    return round(float(10 * np.log10((sig + 1e-9) / (err + 1e-9))), 2)


def run(cmd, env=None):
    e = dict(os.environ)
    for k in ('RUSTY_OPUS_FORCE_MODE', 'FORCE_BW', 'VBR', 'DTX', 'FEC', 'MAXBW', 'SIGNAL'):
        e.pop(k, None)
    if env:
        e.update(env)
    p = subprocess.run(cmd, capture_output=True, text=True, env=e)
    return p.returncode, (p.stdout + p.stderr)


def cell(rate, ch, app, frame, bw, kbps):
    tag = f'{rate}_{ch}_{app}_{frame}_{bw}_{kbps}'
    d = os.path.join(WORK, 'cells')
    os.makedirs(d, exist_ok=True)
    inp = os.path.join(WORK, f'in_{rate}_{ch}.sw')
    ours_bit, lib_bit = os.path.join(d, tag + '.ours.bit'), os.path.join(d, tag + '.lib.bit')
    ours_lib_pcm, ours_ours_pcm = os.path.join(d, tag + '.ol.sw'), os.path.join(d, tag + '.oo.sw')
    lib_lib_pcm = os.path.join(d, tag + '.ll.sw')
    bps = kbps * 1000
    res = dict(rate=rate, ch=ch, app=app, frame=frame, bw=bw, kbps=kbps)

    # libopus reference encode (VBR, default complexity 10 -> ours is 9; routing only)
    la = [OPUS_DEMO, '-e', LIB_APP[app], str(rate), str(ch), str(bps), '-framesize', frame]
    if bw != 'auto':
        la += ['-bandwidth', bw.upper()]
    rc, out = run(la + [inp, lib_bit])
    res['lib_ok'] = rc == 0
    # ours
    env = {'VBR': '1'}
    if bw != 'auto':
        env['FORCE_BW'] = bw
    rc, out = run([ENC, str(rate), str(ch), str(bps), frame, app, inp, ours_bit], env)
    res['ours_ok'] = rc == 0
    res['ours_err'] = '' if rc == 0 else out.strip().splitlines()[-1][:90] if out.strip() else f'exit {rc}'
    if not res['ours_ok']:
        return res
    # libopus decode of OUR stream, with the per-packet range check
    rc, out = run([OPUS_DEMO, '-d', str(rate), str(ch), ours_bit, ours_lib_pcm])
    res['range_ok'] = rc == 0 and 'mismatch' not in out
    res['range_msg'] = '' if res['range_ok'] else out.strip().splitlines()[-1][:90]
    # our decoder on our stream
    rc, out = run([DEC, str(rate), str(ch), ours_bit, ours_ours_pcm], {'RANGECHK': '1'})
    res['ourdec_ok'] = rc == 0 and ' errors=0' in out and 'RANGE: 0 mismatches' in out
    # libopus decode of LIBOPUS's stream (the fidelity anchor)
    if res['lib_ok']:
        run([OPUS_DEMO, '-d', str(rate), str(ch), lib_bit, lib_lib_pcm])
    ref = load(inp, ch)
    ol, oo, ll = load(ours_lib_pcm, ch), load(ours_ours_pcm, ch), load(lib_lib_pcm, ch)
    if ol is not None and oo is not None:
        n = min(len(ol), len(oo))
        res['dec_maxdiff'] = int(np.max(np.abs(ol[:n] - oo[:n]))) if n else -1
        res['dec_len'] = (len(ol), len(oo))
        if res['dec_maxdiff'] > 2:
            # Shifted or genuinely different? Report the first diverging sample
            # and the diff at the best lag in either direction.
            bad = np.nonzero(np.abs(ol[:n] - oo[:n]).max(axis=1) > 2)[0]
            res['dec_first_bad'] = int(bad[0])
            m = int(rate * 0.010)
            if n > 2 * m:
                l1 = best_lag(ol[:n].mean(axis=1), oo[:n].mean(axis=1), m)
                l2 = best_lag(oo[:n].mean(axis=1), ol[:n].mean(axis=1), m)
                d1 = np.abs(ol[:n - l1] - oo[l1:n]).max() if l1 else 1e9
                d2 = np.abs(oo[:n - l2] - ol[l2:n]).max() if l2 else 1e9
                res['dec_lagdiff'] = (l1, int(d1), -l2, int(d2))
    res['snr_ours'] = aligned_snr(ref, ol, rate)
    res['snr_lib'] = aligned_snr(ref, ll, rate) if ll is not None else float('nan')
    co, cl = census(read_bit(ours_bit)), census(read_bit(lib_bit)) if res['lib_ok'] else collections.Counter()
    res['toc_ours'] = co.most_common(1)[0][0] if co else None
    res['toc_lib'] = cl.most_common(1)[0][0] if cl else None
    res['toc_ours_hist'] = {'/'.join(map(str, k)): v for k, v in co.items()}
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--quick', action='store_true', help='one bitrate, audio app only')
    ap.add_argument('--jobs', type=int, default=8)
    a = ap.parse_args()
    if not OPUS_DEMO or not os.path.exists(OPUS_DEMO):
        sys.exit('set OPUS_DEMO to a libopus opus_demo binary')
    for b in (ENC, DEC):
        if not os.path.exists(b):
            sys.exit(f'missing {b}: cargo build --release --example encode_bit --example decode_bit')
    prep_inputs()
    cells = []
    for rate, ch, app, frame, bw in itertools.product(RATES, (1, 2), APPS, FRAMES, BWS):
        if a.quick and app != 'audio':
            continue
        for kbps in ((48,) if a.quick else ((12, 64) if ch == 1 else (24, 96))):
            cells.append((rate, ch, app, frame, bw, kbps))
    print(f'{len(cells)} cells, {a.jobs} jobs', file=sys.stderr)
    out = []
    with cf.ThreadPoolExecutor(a.jobs) as ex:
        def safe(c):
            try:
                return cell(*c)
            except Exception as e:  # an instrument fault is recorded, never fatal
                return dict(zip(('rate', 'ch', 'app', 'frame', 'bw', 'kbps'), c),
                            harness_error=f'{type(e).__name__}: {e}'[:120])
        for i, r in enumerate(ex.map(safe, cells)):
            out.append(r)
            if (i + 1) % 100 == 0:
                print(f'  {i + 1}/{len(cells)}', file=sys.stderr)
    path = os.path.join(WORK, 'matrix.json')
    json.dump(out, open(path, 'w'), indent=0, default=str)
    print(f'wrote {path}', file=sys.stderr)


if __name__ == '__main__':
    main()
