#!/usr/bin/env python3
"""callgrind census: per-function self/incl/calls, self split by inlined file, and edges.
usage: cg.py <file.cg> [topN] [--edges-to SUBSTR] [--edges-from SUBSTR] [--files SUBSTR]"""
import sys, re, collections
path = sys.argv[1]; topn = int(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[2].isdigit() else 40
opt = {sys.argv[i]: sys.argv[i+1] for i in range(2, len(sys.argv)-1) if sys.argv[i].startswith('--')}
names = {}
def nm(tok):
    m = re.match(r'\((\d+)\)(?: (.*))?$', tok)
    if not m: return tok
    if m.group(2) is not None: names[m.group(1)] = m.group(2)
    return names.get(m.group(1), '?')
def short(s):
    s = re.sub(r'^<(.*)>$', r'\1', s)
    s = s.replace('rusty_opus::', '')
    return s[:95]
def shortf(s):
    for p in ('/mnt/e/coding/rusty-opus/',): s = s.replace(p, '')
    s = re.sub(r'^/rustc/[0-9a-f]+/library/', 'std:', s)
    s = re.sub(r'^.*/sysdeps/', 'libc:', s)
    return s[-60:]
self_c = collections.Counter(); byfile = collections.defaultdict(collections.Counter)
calls = collections.Counter(); incl_edge = collections.Counter(); ecalls = collections.Counter()
fn = None; curfile = None; cfn = None; pending = None; total = 0
for line in open(path, errors='replace'):
    line = line.rstrip('\n')
    if not line: continue
    c = line[0]
    if c.isdigit() or c in '+-*':
        parts = line.split()
        if len(parts) < 2: continue
        cost = int(parts[1])
        if pending is not None:
            incl_edge[(fn, pending[0])] += cost; ecalls[(fn, pending[0])] += pending[1]; calls[pending[0]] += pending[1]; pending = None
        else:
            self_c[fn] += cost; byfile[fn][curfile] += cost; total += cost
        continue
    k, _, v = line.partition('=')
    if k == 'fn': fn = nm(v)
    elif k in ('fl', 'fi', 'fe'): curfile = nm(v)
    elif k == 'cfn': cfn = nm(v)
    elif k in ('cfi', 'cfl', 'ob', 'cob'): nm(v)
    elif k == 'calls': pending = (cfn, int(v.split()[0]))
incl = collections.Counter(self_c)
for (a, b), c in incl_edge.items():
    if a != b: incl[a] += c
print(f"TOTAL Ir {total:,}")
if '--edges-to' in opt or '--edges-from' in opt:
    key = opt.get('--edges-to') or opt.get('--edges-from'); idx = 1 if '--edges-to' in opt else 0
    rows = [(c, ecalls[e], e) for e, c in incl_edge.items() if key in e[idx]]
    print(f"{'calls':>10} {'incl':>13} {'per':>9}  caller -> callee")
    for c, n, e in sorted(rows, reverse=True)[:topn]:
        print(f"{n:>10} {c:>13,} {c/max(n,1):>9.1f}  {short(e[0])} -> {short(e[1])}")
elif '--files' in opt:
    for f, _ in self_c.most_common():
        if opt['--files'] in f:
            print(f"{short(f)}  self {self_c[f]:,}")
            for fl, c in byfile[f].most_common(12): print(f"    {c:>13,} {100*c/self_c[f]:5.1f}%  {shortf(fl)}")
else:
    print(f"{'self':>13} {'%':>6} {'incl':>13} {'i%':>6} {'calls':>9} {'self/call':>10}  function")
    for f, c in self_c.most_common(topn):
        n = calls.get(f, 0)
        print(f"{c:>13,} {100*c/total:6.2f} {incl[f]:>13,} {100*incl[f]/total:6.2f} {n:>9} {c/max(n,1):>10.1f}  {short(f)}")
