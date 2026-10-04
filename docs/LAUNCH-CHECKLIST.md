# Launch checklist (r/homelab beta)

What's left before posting to r/homelab, in priority order. This is the
short list; `TESTING-CHECKLIST.md` has the full testing history. Tick items
here as they're done, and add new launch blockers here rather than only in
a chat.

Last reviewed: 2026-10-04.

## Must fix before launch

- [ ] **Large folders.** Found 2026-10-04 by reading `apps/client/src/backup.rs`:
  - Every ~30s cycle reads *every* file in `BACKUP_DIR` fully into memory to
    hash it (`hash_file` → `tokio::fs::read`), even if nothing changed. With
    a few TB the cycle can't finish in 30s and the disk is busy constantly.
  - Sending reads and encrypts each changed file entirely in memory
    (`put_one_file`: `tokio::fs::read` + `age::encrypt`), so a 20 GB file
    needs 20+ GB of RAM to hash and more to send.
  - The receiver rejects any single file over 5 GB (`MAX_FILE_BYTES` in
    `receive.rs`) — ISOs, VM disks and videos are exactly what this
    audience backs up.

  Also: the per-buddy JSON manifest is loaded whole into RAM and rewritten
  in full after every file sent — with a million files that's a 100+ MB
  file rewritten a million times.

  Plan (agreed 2026-10-04), using libraries rather than new machinery:
  1. SQLite index (`rusqlite`, bundled) in the config folder replacing the
     JSON manifests — size, modified time, hash per file; existing installs
     migrated automatically on first start.
  2. Re-read a file only when its size or modified time changed; otherwise
     a cycle is a metadata-only listing.
  3. Stream hash → encrypt → send → receive in small chunks (age's
     streaming encryptor/decryptor, already a dependency), so memory stays
     flat at any file size; raise the 5 GB per-file cap (~100 GB) or remove
     it.
  4. Configurable scan interval (keep ~30s default for small folders).

  Test: two 10 GB files on testa/testb, watching memory and cycle time.

## Should do (quick, ~1 hour total)

- [ ] Setup page + getting-started: tell users plainly to store
      `BACKUP_PASSPHRASE` in a password manager — nobody can recover it,
      not a password reset, not support.
- [ ] Link support@filegarden.net from the login page footer, the status
      page, and the payment-failed email.
- [ ] `backup-buddies-feedback` repo: add `.github/ISSUE_TEMPLATE/config.yml`
      with a `contact_links` entry sending account/billing/login problems to
      support@filegarden.net instead of a public issue.
- [ ] Glance at the last password-reset checks: the "password was changed"
      notice email arrives, and the old password is rejected at login.

## Only if people can pay at launch

- [ ] Payment-failed pause: built and deployed, never exercised against a
      real failed Stripe payment. Run the Stripe CLI test (`stripe trigger
      invoice.payment_failed`) — steps in `TESTING-CHECKLIST.md`, "Needs
      testing — payment-failed relay pause". Can wait if launch is a free
      beta.

## Needed after launch (real gaps, not polish)

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
- [ ] **Exclude patterns.** No way to skip files/folders today (listed under
      "intentional limits" in TESTING-CHECKLIST.md). The `ignore` crate
      (ripgrep's walker) gives `.gitignore`-style excludes and a faster,
      parallel folder walk in one go.

## Fine after launch

- Direct connections across two separate networks (relay fallback is proven,
  so nothing breaks; direct is just faster/cheaper).
- Container start on a host with no IPv6; stale mDNS records after restart.
- Marketplace UI details (stale criteria on private invites, size preselect
  from the dashboard link).
- Per-IP rate limiting (needs Caddy/Cloudflare client-IP header trusted).
- Error messages show container paths (`/backup`) instead of host paths.
- Long-term: Syncthing-style dashboard tiers, auto-derived marketplace
  fields, helpdesk inbox.

## Done (for the record)

Verified live 2026-10-04 unless noted:

- Relay locked to customers' devices; 10 GB/month free relay allowance
  (`c6093a0`).
- Privacy policy page; only strictly necessary cookies, no tracking
  (`680dffa`).
- Rate limiting on login, signup, password reset, verification, invite
  redemption (`cf1a9bc`).
- Account support: self-service password reset, support@filegarden.net, in-app
  Contact support form.
- Client 0.3.x–0.4.0: config / your files / buddy files / restores in separate
  folders; PUID/PGID; refuses to start on an unmounted or wrong buddy-files
  drive; won't mirror an empty backup folder as "delete everything";
  unreadable files/folders no longer abort a cycle or look deleted. Tested on
  TrueNAS over SMB and NFS.
