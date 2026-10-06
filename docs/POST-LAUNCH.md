# After-launch checklist

Work to do once the r/homelab beta is live. Nothing here blocks launch —
those items live in `LAUNCH-CHECKLIST.md`. Move things here from there once
they stop blocking, and tick them as they ship.

Last reviewed: 2026-10-04.

## Real gaps (do first)

- [ ] **Chunked, rsync-like transfers.** Today any change to a file re-sends
      the whole file — a 20 GB VM image with one changed block costs 20 GB
      (billed when it goes over the relay). Classic rsync can't work here:
      the buddy only holds ciphertext and can't read it, and re-encrypting
      changes every byte. The encrypted equivalent (restic/borg/duplicacy):
      split files into content-defined chunks (`fastcdc` crate), encrypt
      each chunk separately, send only chunks the buddy doesn't have.
      Also gives dedup across files and no per-file size limit. Big change:
      new storage format on the buddy side (chunk store instead of one
      file per file), so restore, Browse files, versions, reconciliation
      and pledge accounting all change. Alternative to evaluate first:
      `rustic_core` (Rust library behind rustic, a restic reimplementation)
      as the engine, with the buddy as its storage backend.
- [ ] **Resume interrupted transfers.** A dropped connection mid-file
      restarts that file from zero — painful for 100 GB files over a relay.
      Falls out of chunking above (already-sent chunks don't re-send).
- [ ] **Instant change pickup.** Live change notifications (`notify` crate:
      inotify/FSEvents) on top of the periodic scan, so new files go out in
      seconds without frequent full scans. Doesn't work reliably on NFS/SMB
      mounts, so the periodic scan stays as the fallback.
- [x] **Per-account salt for the encryption key — done in client 0.7.0
      (2026-10-06).** The key is now salted with the account id (fetched
      from `GET /devices/me`, cached in DATA_DIR), so each account's key
      has to be attacked separately. Files sent by 0.5–0.6 (fixed salt)
      still restore; they're only re-encrypted if they change.
- [ ] **Files encrypted before 0.5.0 restore at the old speed** (~1.5 s
      each — scrypt per file). Fine for now; optionally re-encrypt them in
      the background later.
- [ ] **File lists are built whole in memory** on both sides (chunked on
      the wire). Fine to roughly a million files; past that, page through
      the index instead.
- [ ] **Exclude patterns.** No way to skip files/folders today (listed under
      "intentional limits" in TESTING-CHECKLIST.md). The `ignore` crate
      (ripgrep's walker) gives `.gitignore`-style excludes and a faster,
      parallel folder walk in one go.

## Polish / nice to have

- Direct connections across two separate networks (relay fallback is proven,
  so nothing breaks; direct is just faster/cheaper).
- Container start on a host with no IPv6; stale mDNS records after restart.
- Marketplace UI details (stale criteria on private invites, size preselect
  from the dashboard link).
- Per-IP rate limiting (needs Caddy/Cloudflare client-IP header trusted).
- ~~Error messages show container paths (`/backup`) instead of host paths~~ —
  client 0.7.3 uses `BACKUP_DIR_HOST` / `BUDDY_FILES_DIR_HOST` when set,
  otherwise says to look in docker-compose.yml.
- ~~A buddy card with one permanently-stuck file (e.g. over the pledge)
  shows a red "backup failed" pill~~ — done in client 0.7.0: amber
  "N file(s) don't fit" when every refusal is pledge/disk full.
- ~~Dashboard restores log nothing on success~~ — client 0.7.3 logs
  every dashboard restore, success or failure.
- Long-term: Syncthing-style dashboard tiers, auto-derived marketplace
  fields, helpdesk inbox.
