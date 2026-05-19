#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"
pandoc main.md \
  --bibliography=references.bib \
  --citeproc \
  --pdf-engine=xelatex \
  --variable=geometry:margin=1in \
  --variable=fontsize:11pt \
  --variable=linestretch:1.2 \
  -o output.pdf
echo "Built paper/output.pdf"
