# Checklist

The one list of what's left, since launch (r/homelab beta, 2026-10).
On 2026-10-13 this replaced `LAUNCH-CHECKLIST.md` and `POST-LAUNCH.md`
(both in git history) and took over every open item from
`TESTING-CHECKLIST.md`, which is now history and design notes only. New
work, bugs and tests to run go here; tick them as they're done, and move a
finished item's write-up to `TESTING-CHECKLIST.md` if it's worth keeping.

Last reviewed: 2026-10-13.

## Bugs

- [x] **Private invites still sent the listing fields** (fixed
      2026-10-13): the form now sends them only when "List this publicly"
      is ticked, and `POST /invites` stores none of them for a private
      invite.

- [ ] **A buddy's old device identity leaves an orphaned folder forever.**
      When a buddy's client gets a new node id (config folder lost, a
      reinstall, or moving it to another machine with the same token, as
      testb was on 2026-10-13), what the old id stored stays in
      `<buddy-files>/<old node id>/`: it counts against your disk, never
      expires, and shows as a nameless buddy card ("f779826288a4…",
      "no cycle yet", "Their pledge to you: 628.2 MB / 0 B"), because the
      dashboard lists every folder in buddy-files (`dashboard.rs`). Fix
      options: expire a folder whose node id hasn't been one of your
      buddies' devices for 30 days (like backup copies), or let the
      new device of the same account adopt it, since the data is
      encrypted to the account's key, not the device's.

## Before charging money

Billing works for subscribing and cancelling (verified 2026-10-12, flat
$3/month plan). These paths haven't run against real Stripe events yet.

- [ ] **Payment-failed pause, live.** Steps in `TESTING-CHECKLIST.md`,
      "Needs testing — payment-failed relay pause" (`stripe trigger
      invoice.payment_failed`). Check:
  - a failure with `next_payment_attempt` set (Stripe still retrying)
    pauses nothing;
  - a failure with `next_payment_attempt: null` pauses (`relay_status`
    `'paused'`, reason `payment_failed`) and the email arrives;
  - the dashboard banner shows, and clears on `invoice.payment_succeeded`;
  - the paused account's client logs "syncing is paused" and stops (no
    sends, receives, pings or reconciles);
  - its `relay_usage_events` stay flat even if an old client keeps
    reporting;
  - admin: "paused Nd" badge, "Paused longest first" sort, "Paused only"
    filter, and the account-detail note.
- [ ] **Metered relay past 10 GB** on a paid account shows up as a charge on
      the next invoice.
- [ ] Payment-failed handling is manual after the first email (no
      reminders, no auto-cancel). If paused accounts pile up, add a digest
      like `jobs/stale-account-digest.js` (accounts paused over N days)
      rather than automatic cancellation.

## Tests to run live

### Networking

Same-host testa/testb can't answer these, and they need two different
setups.

**Two machines on the same LAN:**

- [x] Do long transfers still drop? **No** (2026-10-13): testa on
      `backupbuddy`, testb moved to a clone (`backupbuddy2`), both on the
      default port 11235 with debug logging. Three runs, ~21 GB (2.8 GB in
      53 s, 7.3 GB in 139 s, 8.1 GB in 155 s), all direct over the LAN; no
      `LastOpenPath`, no abandoned paths, no path through `172.17.0.1`,
      reconcile matched afterwards. See "Known issues".
- [ ] A container restart doesn't leave a stale mDNS record (the old one
      expires on the other side, `DiscoveryEvent::Expired`).

**Two networks, one with a router you control** (e.g. home with
11235/udp forwarded to one client, the other on a phone hotspot):

- [ ] Direct connection with `SYNC_PORT` forwarded. testc (2026-10-03)
      couldn't test this: no access to that network's firewall.
- [ ] LAN discovery (mDNS) finds nothing across networks and changes
      nothing there.
- [x] Without a forward: relay-only, works as before. Confirmed with testc
      on a separate network (2026-10-03).

### Client (testa/testb)

- [ ] 0.14.0, hidden names:
  - a 130-character name shows in Activity once, not every cycle;
  - testb's own dashboard and logs show only encrypted names for what it
    stores;
  - a fresh testa config with the same passphrase and account restores
    with real names.
- [ ] 0.13.0, Activity card lines: restart ("Started, version …"), a
      change (and "Stored 1 file … with you" on testb), delete and exclude,
      testb stopped then back ("Working again"), a restore on both sides,
      wrong then right password, exclude list saved on the dashboard vs. by
      hand, and the raw log tab matching `docker compose logs`.
- [ ] 0.8.1: after a LAN backup (even one that drops), the Bandwidth card
      shows it as Direct and no relay usage reaches the API.
- [ ] 0.8.0: restart testa after "connection dropped mid-file — finishing
      the local copy"; the upload still resumes.
- [ ] Pledge enforcement and the disk-space floor: pledges come only from
      the invite flow, so delete the testa/testb pairing and re-pair at
      30–35 GB (against testb's ~48 GB disk) to hit the refusals.

### Website

- [ ] Find a buddy: the speed filter (Any / 10+ / 50+) shows the right
      listings, and the count line names the speed.
- [ ] The dashboard's "Browse buddies" link preselects its size on Find a
      buddy (`?pledged_bytes=`).
- [ ] Password change: the "password was changed" email arrives; the old
      password is refused and the new one works.
- [ ] Reply to a support email in Purelymail: the quote holds only the
      user's message. If the account details come along too, turn on
      Roundcube's "remove original signature when replying".

## Known issues

- [ ] **Long transfers drop between two clients on the same machine.**
      Not seen between separate machines: 2026-10-13, ~21 GB over three
      runs between two VMs on one LAN, both on the default port, ran clean
      (see "Tests to run live"). Seen twice (2026-10-06/07) with testa and
      testb on one host: minutes into a multi-GB upload, noq's path idle
      timer closes the only IP path (`failed closing path
      err=LastOpenPath`, from `PathTimer::PathIdle`) and the connection
      times out ~11 s later, both times just after testb's mDNS record was
      re-sighted; the selected path was the Docker bridge
      (`172.17.0.1→172.17.0.1`), with IP paths opening and closing every
      few seconds. Every client still offers its `docker0` address as a
      candidate (seen in the 2026-10-13 logs too), but between separate
      machines that path was never tried. Running two clients on one host
      is a test setup, not something users do, so this stays low priority;
      filtering bridge addresses out of the candidates is the fix if it
      ever shows up elsewhere. Cost when it happens: one cycle plus a
      resume (0.8.0), not booked as relay (0.8.1).

## Backlog

Build when there's a reason to; roughly in order of value.

- [ ] **Send only changed parts of files (chunked transfers), next.**
      Decided 2026-10-13 to build it (it was parked on 2026-10-10). Any
      change re-sends the whole file (a 20 GB VM image with one changed
      block costs 20 GB, billed over the relay), and so does a rename or
      move: seen 2026-10-13, renaming four ISOs on testa re-sent ~15 GB,
      and reorganizing a photo folder does the same. Rsync itself can't
      work (the buddy only holds ciphertext); the encrypted equivalent
      (restic/borg/duplicacy) splits files into content-defined chunks
      (`fastcdc`), encrypts each, and sends only chunks the buddy doesn't
      have, so renames, moves and edits cost only what changed, with dedup
      across files and resumable restores. Plan:
  - **Spike first** on a branch: `rustic_core` with the buddy as its
    storage backend vs. our own `fastcdc` chunk store. Measure savings on
    real files and how much of the hard parts below a library covers.
  - **Coexist with today's format, never replace it.** Files already on
    a buddy stay whole-file; only new or changed files go to the chunk
    store; restore reads both. Old files must stay readable forever (like
    old keys in `crypto.rs`).
  - **Negotiate per buddy** by protocol version (as PutStream and
    PutResumable were): older buddies keep getting whole files.
  - **Hard parts:** pledge and disk accounting with shared chunks;
    deleting a chunk only once no live file or kept version uses it (the
    30-day copies); reconciliation, Browse files and "What your buddy
    sees" assume one stored file per file.
  - **Chunk ids are keyed hashes** (from the account key), never plain
    content hashes, or a buddy could test whether you hold a known file.
    Site copy on what buddies see changes too (chunking hides exact file
    sizes).
  - **Opt-in first** (e.g. `CHUNKED_TRANSFERS=on`) for a release or two,
    restores included, then the default.
- [ ] **Instant change pickup.** Change notifications (`notify` crate) on
      top of the periodic scan, so new files go out in seconds. They aren't
      reliable on NFS/SMB, so the scan stays as the fallback.
- [ ] **Different folders for different buddies.** Today every buddy gets
      the whole backup folder (status page limitation): each extra buddy
      is another full upload and more relay. Needs a folder-to-buddy
      mapping in the client config and the index.
- [ ] **Content-Security-Policy** with real script sources: the one open
      item from the 2026-10-12 security review. Every page's inline
      `<script>` has to move into a file first.
- [ ] **Restores resume.** Uploads of files ≥ 256 MB resume since 0.8.0;
      an interrupted restore still starts over. Chunked transfers (above)
      would cover this.
- [ ] **Restore progress outside the modal.** Closing the restore modal
      doesn't stop the restore, but the progress is only visible there
      until "Files from <buddy>" is reopened. A small indicator on the
      buddy card would do.
- [ ] **Re-send warning beyond the local dashboard** (0.12.0 shows it
      there only): on the web dashboard or by email.
- [ ] **File lists built whole in memory.** 0.5.0's `index.sqlite` keeps
      the index on disk between cycles, but `idx.sent_entries()` loads
      every sent row per cycle and per reconcile, and the buddy's file
      list is built whole (`receive::list_sender_files`) and collected
      whole by the sender, though it's chunked on the wire. Fine to about a
      million files; past that, page through both.
- [ ] **Listener reachability check** (like Syncthing's): confirm
      `SYNC_PORT` is reachable from outside. Needs a design first: a buddy
      dial-back message (new in `protocol.rs`, proves it from one network
      only) or a probe on the relay.
- [ ] **Helpdesk inbox** if support volume grows: point `SUPPORT_EMAIL` at
      one (e.g. FreeScout or Zammad). No code change; the email already
      carries the account details.

## Standing rules

- Support: act on account changes only when they're requested from the
  account's registered email address. Never ask a user for their password
  or `BACKUP_PASSPHRASE`.
- `PLEDGE_GB_OPTIONS` in `apps/web/pledge-options.js` is kept by hand.
  Change it whenever the API's `PLEDGE_STEP_BYTES` or `PLEDGE_MAX_BYTES`
  changes (`routes/invites.js`).
