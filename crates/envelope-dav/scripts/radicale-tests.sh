#!/usr/bin/env bash
# Runs the envelope-dav integration tests against a throwaway Radicale 3
# server: starts it on 127.0.0.1 (port 15232 unless RADICALE_PORT is set)
# with a temporary storage folder and `--auth-type none`, runs
# `cargo test -p envelope-dav --test radicale`, then stops the server and
# removes the storage again. Extra arguments are passed to the test binary.
#
#   crates/envelope-dav/scripts/radicale-tests.sh
#   crates/envelope-dav/scripts/radicale-tests.sh --nocapture
set -euo pipefail

host=127.0.0.1
port="${RADICALE_PORT:-15232}"
storage="$(mktemp -d -t envelope-dav-radicale.XXXXXX)"
server_pid=""

cleanup() {
    if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -rf "$storage"
}
trap cleanup EXIT INT TERM

if command -v radicale >/dev/null 2>&1; then
    radicale=(radicale)
else
    radicale=(python3 -m radicale)
fi

if (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; then
    echo "error: something is already listening on $host:$port (set RADICALE_PORT)" >&2
    exit 1
fi

# `--config ""` ignores system and user configuration files.
"${radicale[@]}" --config "" \
    --server-hosts "$host:$port" \
    --storage-filesystem-folder "$storage/collections" \
    --auth-type none \
    --logging-level warning &
server_pid=$!

for _ in $(seq 1 100); do
    if (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; then
        break
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
        echo "error: Radicale exited during startup" >&2
        exit 1
    fi
    sleep 0.1
done

export ENVELOPE_TEST_RADICALE_URL="http://$host:$port/"
cd "$(dirname "$0")/.."
cargo test -p envelope-dav --test radicale -- "$@"
