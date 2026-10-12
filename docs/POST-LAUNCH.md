# After-launch checklist

Work to do once the r/homelab beta is live. Nothing here blocks launch —
those items live in `LAUNCH-CHECKLIST.md`. Move things here from there once
they stop blocking, and tick them as they ship.

Last reviewed: 2026-10-04.

## Real gaps (do first)

- [x] **Exclude patterns.** Client 0.10.0: Syncthing-style
      `DATA_DIR/excludes.txt` (re-read every cycle) plus a built-in list of
      poor fits (PST/OST, live DBs, VM disks, temp/OS clutter);
      `DEFAULT_EXCLUDES=off` drops it. Built on `globset` rather than the
      `ignore` crate: Syncthing's first-match-wins order (so `!(?i)*.pst` can
      override a default) isn't gitignore's last-match-wins. 0.11.0 added
      the dashboard editor, behind the new dashboard login.
- [x] **Warn about big files that keep re-sending.** Client 0.12.0: the
      index logs each re-send (a file already on the buddy sent again);
      the dashboard lists files with 3+ re-sends and 1 GB+ in the past
      week, with a one-click exact-path exclude. Thresholds are constants
      in `dashboard.rs` (RESEND_*). Not done: warning anywhere but the
      local dashboard (e.g. the web dashboard or an email).
- [ ] **Chunked, rsync-like transfers. Revisit only if usage shows it's
      needed** (decided 2026-10-10): typical files for an off-site backup
      (photos, documents, exports) are written once, so whole-file sends
      are close to optimal for them, and running VM disks and live
      databases shouldn't be backed up this way anyway (the copy can be
      inconsistent). Signal to watch: a few accounts making up most of the
      monthly relay bytes in admin stats, or users asking. If it comes up,
      start with a rustic_core spike on a branch (end of this entry).
      Today any change to a file re-sends the whole file — a 20 GB VM image with one changed block costs 20 GB
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
- [x] **"Delete permanently" in Browse files — client 0.9.0
      (2026-10-07).** Protocol v4 PurgeDeleted; the buddy removes a path's
      copies only if there's no live file. Confirmation pop-up first.
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

## Polish / nice to have

- Direct connections across two separate networks (relay fallback is proven,
  so nothing breaks; direct is just faster/cheaper).
- Container start on a host with no IPv6; stale mDNS records after restart.
- Marketplace UI details (stale criteria on private invites, size preselect
  from the dashboard link).
- Per-IP rate limiting (needs Caddy/Cloudflare client-IP header trusted).
- From the pre-public security review (2026-10-12; the XSS, Google
  pre-account takeover, unverified admin and login timing findings were
  fixed then). Still open, all low:
  - Security headers in the Caddyfile: `frame-ancestors 'none'` /
    `X-Frame-Options: DENY` and `nosniff` are easy; a real
    Content-Security-Policy means moving every page's inline script into
    files first.
  - A malformed UUID in a URL (`/devices/:id`, `/invites/public/:id`) or
    the session cookie makes Postgres throw 22P02, answered as a 500
    rather than 400/404. Log noise, not a hole.
  - Per-IP limits can be dodged by hitting app-1 directly with a forged
    `CF-Connecting-IP` (DEPLOY §24 has the firewall fix).
- Annual plan ($30/year, alongside the flat $3/month from DEPLOY §30): cuts
  Stripe's ~$0.39 fixed fee per charge from ~13% to ~4%. Blocked on mixing
  intervals: the relay item is a monthly metered price on the same
  subscription, and stripe-node 16 can't put a yearly and a monthly price
  on one subscription. Needs a Stripe API/SDK upgrade with mixed-interval
  subscriptions, or a separate monthly relay-only subscription.
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
