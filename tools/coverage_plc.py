#!/usr/bin/env python3
"""Packet-loss-concealment parity sweep: libopus-encoded streams decoded with a
deterministic loss pattern by libopus (SIMD build), libopus (scalar build, the
control arm) and rusty-opus, all at the stream's own rate.

PLC is not normative (RFC 6716 leaves it to the implementation), and float PLC
is ill-conditioned (the concealment LPC amplifies float-order noise), so the
yardstick is the control arm: libopus disagreeing with ITSELF across builds.
A cell is "within control" when ours is no farther from the nearer libopus build
than the two builds are from each other: min(ours-vs-SIMD, ours-vs-scalar) <=
max(2, scalar-vs-SIMD).

Requires the streams from tools/coverage_decoder.py (target/coverage_decoder).
Usage: OPUS_DEMO=<simd opus_demo> OPUS_DEMO_SCALAR=<scalar opus_demo> \
       python tools/coverage_plc.py [--jobs N]
"""
import argparse, concurrent.futures as cf, glob, json, os, random, subprocess
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
EX = os.path.join(ROOT, 'target', 'release', 'examples')
DEC = os.path.join(EX, 'decode_bit.exe' if os.name == 'nt' else 'decode_bit')
SRC = os.path.join(ROOT, 'target', 'coverage_decoder')
WORK = os.path.join(ROOT, 'target', 'coverage_plc')
DEMO = os.environ.get('OPUS_DEMO', '')
DEMO_C = os.environ.get('OPUS_DEMO_SCALAR', '')


def npackets(bit):
    d = open(bit, 'rb').read()
    i = n = 0
    while i + 8 <= len(d):
        ln = int.from_bytes(d[i:i + 4], 'big')
        i += 8 + ln
        n += 1
    return n


def pattern(n, seed):
    # ~8% scattered single losses + a few 2-5 packet bursts; never the first
    # two packets or the last one (opus_demo cannot conceal a trailing loss).
    r = random.Random(seed)
    lost = [0] * n
    for k in range(2, n - 1):
        if r.random() < 0.08:
            lost[k] = 1
    for _ in range(max(1, n // 60)):
        s = r.randrange(2, max(3, n - 7))
        for k in range(s, min(n - 1, s + r.randrange(2, 6))):
            lost[k] = 1
    return lost


def load(p, ch):
    if not os.path.exists(p) or os.path.getsize(p) == 0:
        return None
    a = np.fromfile(p, dtype=np.int16).astype(np.int32)
    return a[: len(a) // ch * ch].reshape(-1, ch)


def cell(bit):
    name = os.path.basename(bit)[:-4]
    # <clip>.wav_<rate>_<app>_<kbps>_<frame>
    clip, rest = name.split('.wav_')
    rate, app, kbps, frame = rest.split('_')
    rate = int(rate)
    ch = 2 if clip.endswith('_st') else 1
    n = npackets(bit)
    lf = os.path.join(WORK, name + '.loss')
    open(lf, 'w').write('\n'.join(map(str, pattern(n, hash(name) & 0xffff))) + '\n')
    outs = {}
    for tag, cmd, env in (
        ('simd', [DEMO, '-d', str(rate), str(ch), '-lossfile', lf, bit], None),
        ('scal', [DEMO_C, '-d', str(rate), str(ch), '-lossfile', lf, bit], None),
        ('ours', [DEC, str(rate), str(ch), bit], {'LOSSFILE': lf}),
    ):
        o = os.path.join(WORK, f'{name}.{tag}.sw')
        e = dict(os.environ)
        if env:
            e.update(env)
        subprocess.run(cmd + [o], capture_output=True, env=e)
        outs[tag] = load(o, ch)
    res = dict(stream=name, rate=rate, ch=ch, app=app, kbps=int(kbps), frame=frame,
               lost=sum(pattern(n, hash(name) & 0xffff)), packets=n)
    a, c, o = outs['simd'], outs['scal'], outs['ours']
    if a is None or o is None or c is None:
        res['err'] = 'missing output'
        return res
    m = min(len(a), len(c), len(o))
    res['len'] = (len(a), len(c), len(o))
    res['ctrl'] = int(np.abs(a[:m] - c[:m]).max())
    res['ours'] = int(np.abs(a[:m] - o[:m]).max())
    res['ours_scalar'] = int(np.abs(c[:m] - o[:m]).max())
    # Within control: ours is no farther from the NEARER libopus build than the
    # two libopus builds are from each other.
    res['ok'] = min(res['ours'], res['ours_scalar']) <= max(2, res['ctrl'])
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--jobs', type=int, default=os.cpu_count())
    a = ap.parse_args()
    os.makedirs(WORK, exist_ok=True)
    bits = sorted(glob.glob(os.path.join(SRC, '*.bit')))
    with cf.ThreadPoolExecutor(a.jobs) as ex:
        rows = list(ex.map(cell, bits))
    json.dump(rows, open(os.path.join(WORK, 'plc_matrix.json'), 'w'), indent=1)
    ok = [r for r in rows if r.get('ok')]
    exact = [r for r in rows if min(r.get('ours', 99), r.get('ours_scalar', 99)) <= 2]
    print(f'{len(rows)} streams with loss: {len(exact)} exact (<=2 vs a libopus build), '
          f'{len(ok)} within libopus self-disagreement, {len(rows) - len(ok)} outside')
    for r in rows:
        if not r.get('ok'):
            print(f"  {r['stream']:46s} lost={r.get('lost')}/{r.get('packets')} "
                  f"ours={r.get('ours')} ours_scalar={r.get('ours_scalar')} ctrl={r.get('ctrl')} {r.get('err', '')} len={r.get('len')}")


if __name__ == '__main__':
    main()
