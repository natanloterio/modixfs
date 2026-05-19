#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"

# Always produce LaTeX source (arXiv accepts .tex directly)
pandoc main.md \
  --bibliography=references.bib \
  --citeproc \
  --standalone \
  --variable=geometry:margin=1in \
  --variable=fontsize:11pt \
  --variable=linestretch:1.2 \
  -o output.tex
echo "Built paper/output.tex"

# Produce PDF if a TeX engine is available
if command -v xelatex >/dev/null 2>&1; then
  xelatex -interaction=nonstopmode output.tex >/dev/null 2>&1
  echo "Built paper/output.pdf (via xelatex)"
elif command -v pdflatex >/dev/null 2>&1; then
  pdflatex -interaction=nonstopmode output.tex >/dev/null 2>&1
  echo "Built paper/output.pdf (via pdflatex)"
else
  echo "No TeX engine found — install texlive-xetex for PDF output. LaTeX source is in output.tex."
fi
