#!/usr/bin/env python3
"""call-site census: sites.py <file.cg> <caller-substr> <callee-substr>  -> (file:line) calls incl"""
import sys, re, collections
names = {}
def nm(tok):
    m = re.match(r'\((\d+)\)(?: (.*))?$', tok)
    if not m: return tok
    if m.group(2) is not None: names[m.group(1)] = m.group(2)
    return names.get(m.group(1), '?')
fn = cur = cfn = None; last = 0; pend = None; out = collections.Counter(); inc = collections.Counter()
for line in open(sys.argv[1], errors='replace'):
    line = line.rstrip('\n')
    if not line: continue
    c = line[0]
    if c.isdigit() or c in '+-*':
        p = line.split()
        ln = last if p[0] == '*' else (last + int(p[0]) if p[0][0] in '+-' else int(p[0])); last = ln
        if pend is not None:
            if sys.argv[2] in (fn or '') and sys.argv[3] in (cfn or ''):
                out[(cur.split('/')[-1], ln, cfn[:60])] += pend; inc[(cur.split('/')[-1], ln, cfn[:60])] += int(p[1])
            pend = None
        continue
    k, _, v = line.partition('=')
    if k == 'fn': fn = nm(v)
    elif k in ('fl', 'fi', 'fe'): cur = nm(v)
    elif k == 'cfn': cfn = nm(v)
    elif k in ('cfi', 'cfl', 'ob', 'cob'): nm(v)
    elif k == 'calls': pend = int(v.split()[0])
for k, n in out.most_common(int(sys.argv[4]) if len(sys.argv) > 4 else 15):
    print(f"{n:>10} calls {inc[k]:>13,} Ir  {k[0]}:{k[1]}  -> {k[2]}")
