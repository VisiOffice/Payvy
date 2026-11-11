#!/usr/bin/env bash
# Both CI jobs need real PDF/OCR checks. Retry transient mirror/DNS failures,
# but fail the job if the required tools still cannot be installed.
set -euo pipefail
for attempt in 1 2 3 4; do
  if sudo apt-get -o Acquire::Retries=3 -o APT::Update::Error-Mode=any update &&
     sudo apt-get -o Acquire::Retries=3 install -y poppler-utils tesseract-ocr tesseract-ocr-eng; then
    exit 0
  fi
  if [ "$attempt" -lt 4 ]; then
    echo "OCR package installation failed (attempt $attempt/4); retrying..." >&2
    sleep "$((attempt * 10))"
  fi
done
echo "Required PDF/OCR tools could not be installed after four attempts" >&2
exit 1
