#!/usr/bin/env bash
# The oracle drift comparison needs a read credential. Without it the job reports
# "not qualified" and ends green; it is not a required check, so it proves nothing.
# Sets `available=true|false` in $GITHUB_OUTPUT. Never print the credential.
set -euo pipefail
out="${GITHUB_OUTPUT:-/dev/null}"
if [ -z "${GETH_XDC_DEPLOY_KEY:-}" ]; then
  echo "::notice title=fork-table-drift not qualified::GETH_XDC_DEPLOY_KEY is unavailable (unset secret or fork PR); the oracle drift comparison was skipped and is NOT proven."
  echo "available=false" >> "$out"
else
  echo "available=true" >> "$out"
fi
