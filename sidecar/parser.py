#!/usr/bin/env python3
"""Extract plain text from a document. Usage: parser.py <path>. Prints text to stdout.

The exit codes are a contract with `crates/worker/src/parser.rs`, which turns them into the
classified `failure_reason` a tenant is shown. Neither side documents the other; change one,
change both.

    0  success — text on stdout
    1  unexpected crash — our bug, never the document's
    2  usage error — also our bug: the worker always passes exactly one argument
    3  unsupported file type — the tenant should convert or upload a different format
    4  unreadable document — right type, but damaged/encrypted content

Only 3 and 4 are the tenant's to act on; 1 and 2 are ours. That split is the whole point of the
codes, so do not widen 4 to cover an internal fault: 4 tells a customer their file is broken, and
reporting our own failure that way blames them for our outage. When in doubt, crash (1).
"""
import sys
import pathlib


def main() -> None:
    if len(sys.argv) != 2:
        sys.stderr.write("usage: parser.py <path>\n")
        sys.exit(2)

    path = pathlib.Path(sys.argv[1])
    ext = path.suffix.lower()

    if ext == ".pdf":
        # Deliberately outside the try: a missing pypdf is a broken deployment, not a broken
        # document, and must surface as a crash (1) rather than as "your file is damaged" (4).
        from pypdf import PdfReader

        try:
            reader = PdfReader(str(path))
            text = "\n".join((page.extract_text() or "") for page in reader.pages)
        # Broad on purpose: pypdf raises a wide, undocumented range of exception types.
        except Exception as e:
            # Encrypted, truncated or malformed PDFs all land here. Re-uploading a good copy is
            # the fix, which is exactly what code 4 tells the tenant.
            sys.stderr.write(f"unreadable pdf: {type(e).__name__}: {e}\n")
            sys.exit(4)
    elif ext in {".txt", ".md"}:
        # Not wrapped: `errors="replace"` means decoding cannot raise, so the only realistic
        # failure is an OSError reading a temp file the worker itself just wrote — ours (1).
        text = path.read_text(encoding="utf-8", errors="replace")
    else:
        # Add e.g. python-docx here for .docx later.
        sys.stderr.write(f"unsupported file type: {ext}\n")
        sys.exit(3)

    sys.stdout.write(text)


if __name__ == "__main__":
    main()
