#!/usr/bin/env bash
# Restores the development mailbox of alex@envelope.test to its seeded state.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=${ENVELOPE_DEVSERVER_DIR:-$HERE/../../.devserver}
rm -rf "$ROOT/home/alex@envelope.test"
python3 "$HERE/seed.py" "$ROOT/home/alex@envelope.test/Maildir" alex@envelope.test >/dev/null
