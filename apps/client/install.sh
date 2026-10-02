#!/usr/bin/env bash
# One-liner installer for the Backup Buddies client:
#
#   curl -fsSL https://app.filegarden.net/client/install.sh | bash
#
# Downloads everything needed to build and run the client (it's served
# directly from the app, not from the project's private GitHub repo, so
# this works without any GitHub access). Creates ./backup-buddies-client/
# in the current directory and leaves you with instructions, never starts
# anything on its own.

set -euo pipefail

if ! command -v docker >/dev/null 2>&1; then
  echo "Error: docker is not installed. Install Docker first: https://docs.docker.com/get-docker/" >&2
  exit 1
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "Error: 'docker compose' isn't available (you may only have the older standalone docker-compose)." >&2
  echo "Install the Compose plugin: https://docs.docker.com/compose/install/" >&2
  exit 1
fi

BASE_URL="https://app.filegarden.net/client"
DEST="backup-buddies-client"

if [ -d "$DEST" ]; then
  echo "Error: ./$DEST already exists — remove it or run this from a different directory." >&2
  exit 1
fi

mkdir -p "$DEST/src" "$DEST/assets"
cd "$DEST"

echo "Downloading client files from $BASE_URL ..."
for f in docker-compose.yml .env.example Dockerfile Cargo.toml Cargo.lock; do
  curl -fsSL -o "$f" "$BASE_URL/$f"
done

# Every module main.rs pulls in via `mod ...;` — Caddy serves the whole
# apps/client/ tree statically but doesn't offer directory listing, so this
# has to be an explicit list rather than something discovered at runtime.
# Keep this in sync with apps/client/src/*.rs — a new module there needs
# adding here too, or customers' builds will fail on a missing `mod`.
SRC_FILES="main.rs backup.rs bandwidth.rs dashboard.rs manifest.rs netinfo.rs protocol.rs receive.rs restore.rs"
for f in $SRC_FILES; do
  curl -fsSL -o "src/$f" "$BASE_URL/src/$f"
done

# dashboard.rs embeds these via include_str! at compile time — same
# keep-in-sync caveat as SRC_FILES above, against apps/client/assets/*.
ASSET_FILES="favicon.svg mark.svg"
for f in $ASSET_FILES; do
  curl -fsSL -o "assets/$f" "$BASE_URL/assets/$f"
done

cp .env.example .env

cat <<'EOF'

Downloaded to ./backup-buddies-client/

Next steps:
  1. Edit backup-buddies-client/.env — set DEVICE_TOKEN (from your
     dashboard's Devices card) and BACKUP_PASSPHRASE (something only you
     know; it never leaves this machine and can't be recovered if lost).
  2. cd backup-buddies-client && docker compose up -d
     (builds the image locally the first time — a few minutes)
  3. docker compose logs -f

EOF
