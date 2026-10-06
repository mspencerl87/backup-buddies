# Launch checklist (r/homelab beta)

What's left before posting to r/homelab, in priority order. This is the
short list; `TESTING-CHECKLIST.md` has the full testing history and
`POST-LAUNCH.md` has everything that can wait until after launch. Tick items
here as they're done, and add new launch blockers here rather than only in
a chat.

Last reviewed: 2026-10-06. **Launched** — everything below is done except
the Stripe test, which only matters once people pay. New work goes in
`POST-LAUNCH.md`.

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

## Done 2026-10-04 (quick wins)

- [x] Setup page + getting-started: warning box to store `BACKUP_PASSPHRASE`
      in a password manager — nobody can recover it, not a password reset,
      not support.
- [x] support@filegarden.net linked in every page footer (login and status
      page included) and in the payment-failed, relay-limit and
      stale-account emails (replies go to support).
- [x] `backup-buddies-feedback` repo: `contact_links` send account / login /
      billing problems to Contact support or support@ instead of a public
      issue; README says the same.

## Still to do before launch (all quick, you on the live site)

- [x] Password reset last checks — confirmed working by Spencer
      (2026-10-04).
- [x] Client 0.5.0 live checks (`TESTING-CHECKLIST.md`, "Client 0.5.0"):
      a folder with many small files backs up and restores — confirmed by
      Spencer with ~970 photos (reported 2026-10-06). The one-time JSON manifest
      import wasn't separately checked in the log.
- [x] Progress during long backups — built in client 0.5.1 (2026-10-04):
      the buddy card shows a live bar while a cycle runs (checking files,
      then sending: files and bytes done, speed, time left, current file).
  - [x] Confirmed live by Spencer (reported 2026-10-06).

## Only if people can pay at launch

- [ ] Payment-failed pause: built and deployed, never exercised against a
      real failed Stripe payment. Run the Stripe CLI test (`stripe trigger
      invoice.payment_failed`) — steps in `TESTING-CHECKLIST.md`, "Needs
      testing — payment-failed relay pause". Can wait if launch is a free
      beta.

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
