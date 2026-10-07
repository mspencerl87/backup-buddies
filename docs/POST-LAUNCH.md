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
- [x] **Resume interrupted transfers — done in client 0.8.0 (2026-10-07)**
      for uploads of files ≥ 256 MB: the sender keeps the encrypted copy
      it's sending and continues from the buddy's partial (protocol v3,
      PutResumable). Restores still restart from zero; chunking above
      would cover both.
- [x] **Backup copies kept for 30 days — client 0.8.2 (2026-10-07).**
      Copies of replaced or deleted files used to stay on the buddy forever
      without counting against the pledge. Now dated when kept and removed
      after VERSION_RETENTION_DAYS (30) by the buddy, at startup and daily.
- [ ] **"Delete permanently" in Browse files** for deleted files' copies,
      so space can be freed before the 30 days are up. Needs a protocol v4
      request (buddy removes a path's copies only if there's no live file);
      about half a day.
- [ ] **Dropped connections between buddies on a local network.** Seen
      twice (2026-10-06/07), testa and testb on the same host
      (`backupbuddy`, host networking, ports 11235/11236): minutes into a
      multi-GB upload, noq's per-path idle timer fires on the only open IP
      path (`failed closing path err=LastOpenPath`, from
      `PathTimer::PathIdle`), and the connection times out ~11 s later.
      Both times shortly after testb's mDNS record was re-sighted. In a
      clean run with `RUST_LOG=…,iroh::_events=debug,iroh::socket::remote_map=debug`
      the selected path was the Docker bridge (`172.17.0.1→172.17.0.1`),
      IP paths were opened and closed (code 62) every few seconds, and a
      relay path did exist. With the socket on 0.0.0.0, iroh offers every
      interface address as a candidate, including `docker0` even when
      DOWN; every Docker host has 172.17.0.1 and every client the same
      default port, so a remote buddy's 172.17.0.1:11235 candidate points
      at your own client. Not fixed: binding to specific addresses would
      stop that but break direct connections if the address changes.
      Since 0.8.0 a drop costs one cycle plus a resume; since 0.8.1 it's no
      longer booked as relay. Next: catch a drop with the debug logging on,
      and try two clients on separate machines. Documented as a known
      limitation (status page, setup troubleshooting, client README).
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
