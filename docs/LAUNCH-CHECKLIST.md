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

  Fix: re-hash only when size or modified time changed (store mtime in the
  manifest); hash, encrypt (age's streaming encryptor) and send in chunks;
  raise or remove the 5 GB cap, streaming on the receive side too. Then
  test with a folder of a few large files (e.g. 2 × 10 GB) and watch
  memory and cycle time.

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
