#!/usr/bin/env bash
# A required oracle comparison cannot qualify when its read credential is absent.
# Never print the credential; the calling workflow performs the pinned fetch next.
set -euo pipefail
if [ -z "${GETH_XDC_DEPLOY_KEY:-}" ]; then
  echo "::error::GETH_XDC_DEPLOY_KEY is unavailable; the pinned oracle drift gate cannot qualify." >&2
  exit 1
fi
