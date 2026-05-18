#!/usr/bin/env bash
# Seed `server/src/files/` with three minimal valid PDFs so the demo can
# serve something out of the box. Each is a single blank page; only the
# MediaBox dimensions differ, which keeps every byte offset stable so the
# xref table stays correct without re-counting per file.
#
# Run once after cloning, before `docker compose up --build`.

set -euo pipefail

cd "$(dirname "$0")/../server/src/files"

# Emit a single minimal PDF to `$1` whose only variable bit is the MediaBox
# (page dimensions) supplied as `$2`. The xref offsets in the template are
# precomputed against an 11-character MediaBox, so we hard-fail if the input
# would shift those offsets.
write_pdf() {
  local out="$1"
  local mediabox="$2" # e.g. "0 0 200 200" — must be 11 chars to keep offsets
  if [[ ${#mediabox} -ne 11 ]]; then
    echo "MediaBox must be exactly 11 chars (got ${#mediabox}: '$mediabox')" >&2
    exit 1
  fi
  printf '%%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Count 1/Kids[3 0 R]>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[%s]/Resources<<>>>>endobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000052 00000 n \n0000000101 00000 n \ntrailer<</Size 4/Root 1 0 R>>\nstartxref\n178\n%%EOF\n' "$mediabox" >"$out"
}

write_pdf sample-square.pdf "0 0 200 200"
write_pdf sample-letter.pdf "0 0 612 792"
write_pdf sample-a4.pdf "0 0 595 842"

ls -la *.pdf
echo "Seeded $(ls *.pdf | wc -l) PDFs into $(pwd)"
