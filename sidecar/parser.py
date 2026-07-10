#!/usr/bin/env python3
"""Extract plain text from a document. Usage: parser.py <path>. Prints text to stdout."""
import sys
import pathlib


def main() -> None:
    if len(sys.argv) != 2:
        sys.stderr.write("usage: parser.py <path>\n")
        sys.exit(2)

    path = pathlib.Path(sys.argv[1])
    ext = path.suffix.lower()

    if ext == ".pdf":
        from pypdf import PdfReader
        reader = PdfReader(str(path))
        text = "\n".join((page.extract_text() or "") for page in reader.pages)
    elif ext in {".txt", ".md"}:
        text = path.read_text(encoding="utf-8", errors="replace")
    else:
        # Add e.g. python-docx here for .docx later.
        sys.stderr.write(f"unsupported file type: {ext}\n")
        sys.exit(3)

    sys.stdout.write(text)


if __name__ == "__main__":
    main()
