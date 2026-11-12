#!/usr/bin/env python3
"""Print an Ir summary, or its delta against a base summary; exit 1 if an anchor moved."""
import sys


def load(path):
    rows = {}
    for line in open(path):
        name, ir, anchor = line.rstrip("\n").split(" ", 2)
        rows[name] = (int(ir), anchor)
    return rows


new = load(sys.argv[1])
base = load(sys.argv[2]) if len(sys.argv) > 2 else None
moved = False
for name, (ir, anchor) in new.items():
    if base is None or name not in base:
        print(f"{name:14s} {ir:>14,}  {anchor}")
        continue
    b_ir, b_anchor = base[name]
    flag = "" if anchor == b_anchor else "   ANCHOR MOVED"
    moved |= anchor != b_anchor
    d = ir - b_ir
    print(f"{name:14s} {ir:>14,}  {d:>+12,}  {100.0 * d / b_ir:>+7.3f}%{flag}")
if moved:
    print("ANCHOR MOVED - the experiment changed, not the code")
    sys.exit(1)
