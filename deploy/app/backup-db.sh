#!/usr/bin/env bash
# Nightly off-site backup of the Postgres database to Backblaze B2.
#
# The VPS provider's backups snapshot the whole disk, but they live and die
# with the server (deleting it, or losing the account, takes them too). Since client
# 0.7.0 the account id is part of every user's encryption key, so losing
# the database would also stop users restoring on a new machine. This keeps
# a plain SQL dump somewhere else.
#
# Reads from deploy/app/.env: B2_KEY_ID, B2_APP_KEY, B2_BUCKET, and
# optionally BACKUP_PING_URL (an Uptime Kuma push monitor, so a night
# without a backup raises an alert). Run by bb-db-backup.timer; setup and
# restore steps are in docs/DEPLOY.md section 26.
set -euo pipefail
cd "$(dirname "$0")"

# Pull single values out of .env rather than sourcing it, so a value with
# quotes or $ in it elsewhere in the file can't break or run anything here.
env_get() {
  { grep -E "^$1=" .env || true; } | tail -n 1 | cut -d= -f2- | sed -e 's/^"//' -e 's/"$//'
}
B2_KEY_ID=$(env_get B2_KEY_ID)
B2_APP_KEY=$(env_get B2_APP_KEY)
B2_BUCKET=$(env_get B2_BUCKET)
BACKUP_PING_URL=$(env_get BACKUP_PING_URL)
for v in B2_KEY_ID B2_APP_KEY B2_BUCKET; do
  [ -n "${!v}" ] || { echo "backup-db: $v is not set in deploy/app/.env" >&2; exit 1; }
done

stamp=$(date -u +%Y-%m-%dT%H%MZ)
file="backup_buddies-$stamp.sql.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

docker compose exec -T postgres pg_dump -U backup_buddies -d backup_buddies --no-owner --no-privileges \
  | gzip -9 > "$tmp/$file"

# pg_dump writes this line last; without it the dump was cut short.
if ! gzip -dc "$tmp/$file" | tail -n 5 | grep -q "PostgreSQL database dump complete"; then
  echo "backup-db: dump looks incomplete, not uploading" >&2
  exit 1
fi

# rclone in a throwaway container, so nothing is installed on the host.
docker run --rm \
  -v "$tmp:/dump:ro" \
  -e RCLONE_CONFIG_B2_TYPE=b2 \
  -e RCLONE_CONFIG_B2_ACCOUNT="$B2_KEY_ID" \
  -e RCLONE_CONFIG_B2_KEY="$B2_APP_KEY" \
  rclone/rclone:1.68.2 copyto "/dump/$file" "b2:$B2_BUCKET/postgres/$file"

echo "backup-db: uploaded $file ($(du -h "$tmp/$file" | cut -f1))"

if [ -n "$BACKUP_PING_URL" ]; then
  curl -fsS -m 10 "$BACKUP_PING_URL" > /dev/null || echo "backup-db: uploaded, but the monitor ping failed" >&2
fi
