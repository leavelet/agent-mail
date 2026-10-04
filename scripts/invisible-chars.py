#!/usr/bin/env python3
"""Print the code point ranges for `is_invisible` in
crates/amail-proto/src/text.rs and web/src/lib/visible.ts.

The set is the union of:
  - every code point with the Unicode property Default_Ignorable_Code_Point
    (DerivedCoreProperties.txt),
  - every code point of general category Cf (UnicodeData.txt),
  - U+2800 BRAILLE PATTERN BLANK, which renders as a blank.

Usage:
  curl -O https://www.unicode.org/Public/17.0.0/ucd/DerivedCoreProperties.txt
  curl -O https://www.unicode.org/Public/17.0.0/ucd/UnicodeData.txt
  scripts/invisible-chars.py DerivedCoreProperties.txt UnicodeData.txt
"""

import sys


def ranges_from_derived(path, prop):
    out = set()
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            cps, name = (p.strip() for p in line.split(";")[:2])
            if name != prop:
                continue
            lo, _, hi = cps.partition("..")
            out.update(range(int(lo, 16), int(hi or lo, 16) + 1))
    return out


def category(path, cat):
    out = set()
    with open(path, encoding="utf-8") as f:
        for line in f:
            fields = line.split(";")
            if fields[2] == cat:
                out.add(int(fields[0], 16))
    return out


def main():
    derived, data = sys.argv[1:3]
    cps = ranges_from_derived(derived, "Default_Ignorable_Code_Point")
    cps |= category(data, "Cf")
    cps.add(0x2800)
    runs = []
    for cp in sorted(cps):
        if runs and runs[-1][1] == cp - 1:
            runs[-1][1] = cp
        else:
            runs.append([cp, cp])
    print("Rust:")
    for lo, hi in runs:
        print(f"    0x{lo:04X}" if lo == hi else f"    0x{lo:04X}..=0x{hi:04X}")
    print("TypeScript:")
    for lo, hi in runs:
        print(f"  [0x{lo:04x}, 0x{hi:04x}],")


if __name__ == "__main__":
    main()
