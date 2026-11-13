#!/usr/bin/env python3
"""Per-function self-cost delta between two callgrind files (attributes a brick's delta).
usage: cgdiff.py <base.cg> <new.cg> [topN]"""
import collections
import re
import sys


def load(path):
    names = {}

    def nm(tok):
        m = re.match(r"\((\d+)\)(?: (.*))?$", tok)
        if not m:
            return tok
        if m.group(2) is not None:
            names[m.group(1)] = m.group(2)
        return names.get(m.group(1), "?")

    self_c = collections.Counter()
    calls = collections.Counter()
    fn = cfn = None
    pending = None
    for line in open(path, errors="replace"):
        line = line.rstrip("\n")
        if not line:
            continue
        c = line[0]
        if c.isdigit() or c in "+-*":
            parts = line.split()
            if len(parts) < 2:
                continue
            if pending is not None:
                calls[cfn] += pending
                pending = None
            else:
                self_c[fn] += int(parts[1])
            continue
        k, _, v = line.partition("=")
        if k == "fn":
            fn = nm(v)
        elif k == "cfn":
            cfn = nm(v)
        elif k in ("fl", "fi", "fe", "cfi", "cfl", "ob", "cob"):
            nm(v)
        elif k == "calls":
            pending = int(v.split()[0])
    return self_c, calls


a, ac = load(sys.argv[1])
b, bc = load(sys.argv[2])
topn = int(sys.argv[3]) if len(sys.argv) > 3 else 15
rows = sorted(((b[f] - a[f], f) for f in set(a) | set(b) if b[f] != a[f]), key=lambda r: -abs(r[0]))
print(f"total {sum(b.values()) - sum(a.values()):+,}")
for d, f in rows[:topn]:
    name = re.sub(r"^<(.*)>$", r"\1", f).replace("rusty_opus::", "")[:80]
    print(f"{d:>+13,}  self {a[f]:>12,} -> {b[f]:>12,}  calls {ac[f]:>8} -> {bc[f]:>8}  {name}")
