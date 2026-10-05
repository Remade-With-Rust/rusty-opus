#!/usr/bin/env python3
"""Project-specific invariant checks that must ignore test code (hardening gate H-22).

Complements .semgrep/rusty-opus.yml. Comments, string literals and `#[cfg(test)]`
modules are stripped before matching, so only library code is checked.

  1. no `.unwrap()` / `.expect(` on the decoder and packet-utility paths: a
     malformed packet must yield an `Error`, never a panic;
  2. CPU feature detection only inside `crate::isa`, so every dispatcher checks
     exactly its kernel's feature set and honours the RUSTY_OPUS_ISA cap;
  3. the environment is read only by `crate::isa` (the documented
     RUSTY_OPUS_ISA cap) and `research_env` (compiled to `None` without the
     `research` feature), and the filesystem is touched only by items gated on
     that feature: a production build's behaviour cannot depend on, or write
     to, anything outside its arguments.

Exit status 1 on any violation.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
NO_UNWRAP = ['src/lib.rs', 'src/repacketizer.rs', 'src/multistream.rs', 'src/range_coder.rs',
             'src/silk/dec_api.rs', 'src/silk/decode_frame.rs', 'src/silk/decode_indices.rs',
             'src/silk/decode_parameters.rs', 'src/silk/decode_pulses.rs', 'src/silk/decode_core.rs']


def strip(src: str) -> str:
    """Blank out comments, string/char literals and #[cfg(test)] items, keeping
    line numbers stable."""
    out, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if src.startswith('//', i):
            j = src.find('\n', i)
            j = n if j < 0 else j
            out.append(' ' * (j - i)); i = j
        elif src.startswith('/*', i):
            j = src.find('*/', i + 2)
            j = n if j < 0 else j + 2
            out.append(re.sub(r'[^\n]', ' ', src[i:j])); i = j
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == chr(92) else 1
            out.append(re.sub(r'[^\n]', ' ', src[i:j + 1])); i = j + 1
        else:
            out.append(c); i += 1
    s = ''.join(out)
    # remove #[cfg(test)] and #[cfg(feature = "research")] items by brace matching
    # (`strip` has already blanked string literals, so `feature = "research"`
    # reads as `feature =` followed by eleven spaces.)
    gated = r'#\[cfg\((?:(?:all\()?test[,)]|feature =\s{11}\))[^\]]*\]'
    for m in reversed(list(re.finditer(gated, s))):
        b = s.find('{', m.end())
        semi = s.find(';', m.end())
        if b < 0 or (0 <= semi < b):
            continue
        depth = 0
        for k in range(b, len(s)):
            if s[k] == '{':
                depth += 1
            elif s[k] == '}':
                depth -= 1
                if depth == 0:
                    s = s[:m.start()] + re.sub(r'[^\n]', ' ', s[m.start():k + 1]) + s[k + 1:]
                    break
    return s


def main() -> int:
    bad = []
    for rel in NO_UNWRAP:
        p = ROOT / rel
        if not p.exists():
            continue
        for ln, line in enumerate(strip(p.read_text(encoding='utf-8')).splitlines(), 1):
            if re.search(r'\.unwrap\(\)|\.expect\(', line):
                bad.append(f'{rel}:{ln}: unwrap/expect on a decode path: {line.strip()}')
    for p in sorted((ROOT / 'src').rglob('*.rs')):
        rel = p.relative_to(ROOT).as_posix()
        if rel == 'src/isa.rs':
            continue
        for ln, line in enumerate(strip(p.read_text(encoding='utf-8')).splitlines(), 1):
            if re.search(r'is_(x86|aarch64)_feature_detected!', line):
                bad.append(f'{rel}:{ln}: CPU feature detection outside crate::isa: {line.strip()}')
    for p in sorted((ROOT / 'src').rglob('*.rs')):
        rel = p.relative_to(ROOT).as_posix()
        for ln, line in enumerate(strip(p.read_text(encoding='utf-8')).splitlines(), 1):
            if re.search(r'std::env::', line) and rel != 'src/isa.rs':
                bad.append(f'{rel}:{ln}: environment read outside crate::isa / research_env: {line.strip()}')
            if re.search(r'std::fs::(?!File\b)|File::(create|open)|OpenOptions', line):
                bad.append(f'{rel}:{ln}: filesystem access outside the research feature: {line.strip()}')
    for b in bad:
        print(b)
    print(f'check_invariants: {len(bad)} violation(s)')
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main())
