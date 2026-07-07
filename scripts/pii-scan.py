#!/usr/bin/env python3
"""Scan files for denylisted terms (docs/demo/denylist.txt), extracting real
text from PDFs first.

A plain grep on a PDF -- even after decompressing its FlateDecode streams --
does not see PDF text encoded through a composite font (CIDFontType0/Type0),
which is what Typst emits: each glyph is a numeric CID, not a literal ASCII
byte, and only the font's ToUnicode map recovers the displayed character.
Byte-level search cannot see through that; pypdf's text extraction can,
since it walks the content stream operators and resolves CIDs properly.
This is a real dependency (`pip install pypdf`), not a shortcut -- a scanner
that only *looks* like it works is worse than no scanner.

Usage: pii-scan.py [PATH ...]
A bare directory argument is walked recursively. Exits 1 and prints every
match (file, term) if anything is found; exits 0 and prints nothing if the
scan is clean; exits 2 if pypdf is missing and a PDF needs scanning.
"""

import os
import sys
from pathlib import Path

# The tracked denylist.txt is a placeholder -- the real terms (name, contact
# info, employer names) never get committed. CI reads them from the
# PII_DENYLIST secret (newline-separated); locally they live in
# denylist.local.txt, which .git/info/exclude keeps out of the repo.
DEMO_DIR = Path(__file__).resolve().parent.parent / "docs" / "demo"
DENYLIST_PATH = DEMO_DIR / "denylist.txt"
LOCAL_DENYLIST_PATH = DEMO_DIR / "denylist.local.txt"


def _parse(text: str) -> list[str]:
    terms = []
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            terms.append(line.lower())
    return terms


def load_denylist() -> list[str]:
    env_value = os.environ.get("PII_DENYLIST")
    if env_value:
        return _parse(env_value)
    if LOCAL_DENYLIST_PATH.exists():
        return _parse(LOCAL_DENYLIST_PATH.read_text())
    print(
        "pii-scan.py: no denylist available -- set PII_DENYLIST or create "
        f"{LOCAL_DENYLIST_PATH} (see docs/demo/denylist.txt)",
        file=sys.stderr,
    )
    raise SystemExit(2)


def pdf_text(path: Path) -> str:
    try:
        from pypdf import PdfReader
    except ImportError:
        print(
            "pii-scan.py: pypdf is required to scan PDF content "
            "(`pip install pypdf`) -- refusing to silently skip a PDF",
            file=sys.stderr,
        )
        raise SystemExit(2)
    reader = PdfReader(str(path))
    return "\n".join(page.extract_text() for page in reader.pages)


def searchable_text(path: Path) -> str:
    if path.suffix.lower() == ".pdf":
        return pdf_text(path)
    return path.read_text(errors="ignore")


_SKIP = {DENYLIST_PATH, LOCAL_DENYLIST_PATH}


def iter_files(paths: list[str]):
    for raw in paths:
        p = Path(raw)
        if p.resolve() in _SKIP:
            continue
        if p.is_dir():
            yield from (f for f in p.rglob("*") if f.is_file() and f.resolve() not in _SKIP)
        elif p.is_file():
            yield p


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: pii-scan.py [PATH ...]", file=sys.stderr)
        return 2

    terms = load_denylist()
    hits = []
    for path in iter_files(sys.argv[1:]):
        text = searchable_text(path).lower()
        for term in terms:
            if term in text:
                hits.append((path, term))

    if hits:
        print("PII LEAK -- denylisted term(s) found:", file=sys.stderr)
        for path, term in hits:
            print(f"  {path}: {term!r}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
