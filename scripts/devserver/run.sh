#!/usr/bin/env bash
# Starts a self-contained local mail + calendar server for developing Envelope:
#   IMAP  127.0.0.1:1143 (STARTTLS) and 127.0.0.1:1993 (TLS, self-signed)
#   SMTP  127.0.0.1:1587 (no TLS, any password)
#   DAV   http://127.0.0.1:5232/ (Radicale CalDAV/CardDAV, any password)
# Accounts: alex@envelope.test / password, sam@envelope.test / password
# Data lives in .devserver/ (delete it to start fresh).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=${ENVELOPE_DEVSERVER_DIR:-$HERE/../../.devserver}
mkdir -p "$ROOT"/run "$ROOT"/state "$ROOT"/home "$ROOT"/radicale
ROOT=$(cd "$ROOT" && pwd)

IMAP_PORT=${IMAP_PORT:-1143}
IMAPS_PORT=${IMAPS_PORT:-1993}
SMTP_PORT=${SMTP_PORT:-1587}
DAV_PORT=${DAV_PORT:-5232}

if [ ! -f "$ROOT/cert.pem" ]; then
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$ROOT/key.pem" -out "$ROOT/cert.pem" \
    -days 3650 -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" 2>/dev/null
fi

: > "$ROOT/users"
for user in alex@envelope.test sam@envelope.test; do
  echo "$user:{PLAIN}password:$(id -u):$(id -g)::$ROOT/home/$user::" >> "$ROOT/users"
done

sed -e "s|@ROOT@|$ROOT|g" -e "s|@USER@|$(id -un)|g" -e "s|@GROUP@|$(id -gn)|g" \
    -e "s|@IMAP_PORT@|$IMAP_PORT|g" -e "s|@IMAPS_PORT@|$IMAPS_PORT|g" \
    "$HERE/dovecot.conf.in" > "$ROOT/dovecot.conf"

if [ ! -d "$ROOT/home/alex@envelope.test/Maildir" ]; then
  python3 "$HERE/seed.py" "$ROOT/home/alex@envelope.test/Maildir" alex@envelope.test
fi

pids=()
cleanup() { kill "${pids[@]}" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

dovecot -F -c "$ROOT/dovecot.conf" & pids+=($!)
python3 "$HERE/smtp_sink.py" "$ROOT" "$SMTP_PORT" & pids+=($!)
python3 -m radicale --server-hosts "127.0.0.1:$DAV_PORT" \
  --storage-filesystem-folder "$ROOT/radicale" --auth-type none \
  --logging-level warning & pids+=($!)

echo "devserver: IMAP $IMAP_PORT/$IMAPS_PORT, SMTP $SMTP_PORT, DAV $DAV_PORT (data: $ROOT)"
wait
