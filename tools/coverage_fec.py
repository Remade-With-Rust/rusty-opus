#!/usr/bin/env python3
"""In-band FEC conformance sweep: rusty-opus streams with LBRR, decoded by libopus.

libopus parses every packet's LBRR section, even when nothing is lost, and checks
the encoder's final range per packet, so any LBRR framing error is caught. Each
cell is decoded twice: normally, and with in-band FEC recovery over a fixed loss
pattern.

The encoder matrix (coverage_matrix.py) runs with FEC off; this covers the other
half of the SILK bitstream.

Usage: OPUS_DEMO=<opus_demo> python tools/coverage_fec.py [--jobs N]
Inputs: target/coverage_matrix/in_<rate>_<ch>.sw (tools/gen_gate_corpus.py).
"""
import argparse, concurrent.futures as cf, itertools, os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
EX = os.path.join(ROOT, 'target', 'release', 'examples')
ENC = os.path.join(EX, 'encode_bit.exe' if os.name == 'nt' else 'encode_bit')
WORK = os.path.join(ROOT, 'target', 'coverage_fec')
INPUTS = os.path.join(ROOT, 'target', 'coverage_matrix')
OPUS_DEMO = os.environ.get('OPUS_DEMO', '')

CELLS = list(itertools.product([8000, 12000, 16000, 24000, 48000], [1, 2], ['voip', 'audio'],
                               ['10', '20', '40', '60'], [12000, 24000, 48000], ['vbr', 'cbr']))


def cell(c, loss):
    rate, ch, app, frame, bitrate, rc = c
    key = '_'.join(map(str, c))
    bit, out = os.path.join(WORK, key + '.bit'), os.path.join(WORK, key + '.sw')
    env = {k: v for k, v in os.environ.items() if k not in ('VBR', 'FEC')}
    env['FEC'] = '1'  # encode_bit: in-band FEC on, 30% expected loss
    if rc == 'vbr':
        env['VBR'] = '1'
    src = os.path.join(INPUTS, f'in_{rate}_{ch}.sw')
    p = subprocess.run([ENC, str(rate), str(ch), str(bitrate), frame, app, src, bit],
                       env=env, capture_output=True, text=True)
    if p.returncode:
        return key, 'encode failed: ' + (p.stdout + p.stderr).strip()[-200:]
    for extra in ([], ['-inbandfec', '-lossfile', loss]):
        p = subprocess.run([OPUS_DEMO, '-d', str(rate), str(ch)] + extra + [bit, out],
                           capture_output=True, text=True)
        if p.returncode:
            tail = (p.stdout + p.stderr).strip().splitlines()
            return key, ('with loss: ' if extra else '') + (tail[-1] if tail else 'failed')
    os.remove(bit)
    os.remove(out)
    return key, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--jobs', type=int, default=os.cpu_count())
    args = ap.parse_args()
    if not OPUS_DEMO:
        sys.exit('set OPUS_DEMO to a libopus opus_demo binary')
    if not os.path.exists(ENC):
        sys.exit(f'missing {ENC}: cargo build --release --example encode_bit')
    os.makedirs(WORK, exist_ok=True)
    loss = os.path.join(WORK, 'loss.txt')
    with open(loss, 'w') as f:
        f.write('\n'.join('1' if (i % 7 == 3 or i % 11 == 5) else '0' for i in range(4000)) + '\n')
    with cf.ThreadPoolExecutor(args.jobs) as ex:
        res = list(ex.map(lambda c: cell(c, loss), CELLS))
    bad = [(k, m) for k, m in res if m]
    print(f'FEC conformance: {len(CELLS) - len(bad)}/{len(CELLS)} cells accepted by libopus '
          '(range-checked, with and without loss)')
    for k, m in bad[:20]:
        print('  !', k, m)
    sys.exit(1 if bad else 0)


if __name__ == '__main__':
    main()
