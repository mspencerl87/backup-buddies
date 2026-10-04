# Launch checklist (r/homelab beta)

What's left before posting to r/homelab, in priority order. This is the
short list; `TESTING-CHECKLIST.md` has the full testing history. Tick items
here as they're done, and add new launch blockers here rather than only in
a chat.

Last reviewed: 2026-10-04.

## Must fix before launch

- [x] **Large folders — done in client 0.5.0 (2026-10-04).** Fixed:
  every cycle re-read and hashed every file; sending held each whole file
  in RAM (twice); 5 GB per-file cap; JSON manifest rewritten in full after
  every file. Two more blockers found while doing it, also fixed:
  - **~1.5 s of CPU per file just to encrypt** (age's passphrase mode runs
    scrypt per file): 20,000 photos ≈ 8 hours, and the same again to
    restore. Now scrypt runs once at startup to derive an X25519 key.
  - **File list broke above ~10,000 files** (one message capped at 1 MB),
    which broke reconciliation, restore and Browse files. Now chunked.

  Now: SQLite index (`rusqlite`), re-read only on size/mtime change,
  streaming everywhere (hash, encrypt, upload, store, download, decrypt),
  1 TB per-file cap, `SCAN_INTERVAL_SECS`, chunked file list, and old
  (≤0.4) buddies keep working via a fallback. Tested end to end with two
  real endpoints: a 6 GB file on a 7 GB-RAM box (peak ~263 MB; unchanged
  cycle 0 ms; restore byte-identical), and 20,000 files (first backup
  41 s, unchanged cycle 0.14 s, restore 23 s).
  - [x] Confirmed live 2026-10-04: real ISOs over SMB → NFS, restored
        byte-identical (sha256 match), ~10 MB memory during transfer.

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

- [ ] **Progress during long backups.** The buddy card only updates when a
      whole cycle finishes, so a first backup of a big folder shows "no cycle
      yet" for minutes or hours with no sign of life (seen with 10 GB of
      ISOs). Show files/bytes done and current file while a cycle runs —
      same shape as the restore progress that already exists. Probably worth
      doing before or right after launch: r/homelab first backups will be
      long.
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
- [ ] **Per-account salt for the encryption key.** 0.5.0 derives the file
      key from the passphrase with a fixed salt, so "passphrase alone
      restores everything" keeps working. Trade-off: an attacker holding
      ciphertext can attack weak passphrases across all users at once.
      Hardening: a random per-account salt (e.g. stored with the account
      and on each buddy), keeping passphrase-only recovery.
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

## Fine after launch

- Direct connections across two separate networks (relay fallback is proven,
  so nothing breaks; direct is just faster/cheaper).
- Container start on a host with no IPv6; stale mDNS records after restart.
- Marketplace UI details (stale criteria on private invites, size preselect
  from the dashboard link).
- Per-IP rate limiting (needs Caddy/Cloudflare client-IP header trusted).
- Error messages show container paths (`/backup`) instead of host paths.
- A buddy card with one permanently-stuck file (e.g. over the pledge) shows
  a red "backup failed" pill even though everything else synced — a softer
  "1 file can't sync" warning style would read better.
- Dashboard restores log nothing on success (only failures), which made a
  "did it run?" question harder to answer — add an info line.
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
