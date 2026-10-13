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

- [x] **A buddy's old device identity leaves an orphaned folder forever.**
      Fixed in client 0.20.0: once a day, against a successful buddy
      poll, a folder whose node id isn't any buddy's device is marked
      (`DATA_DIR/orphans.json`), removed 30 days later unless the device
      comes back, and its pledge usage freed; the card says when
      (`receive::sweep_orphans`, unit-tested). Covers ended pairings too.
      Not done: adopting the old folder for the account's new device.
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

- [ ] 0.20.0: testa's Activity shows "Moved to a chunked backup: N
      files sent the old way are kept as backup copies for 30 days", and
      a chunked backup line lists its files when clicked.
- [ ] 0.20.0: within a day, testa's card for the old testb identity
      (`f779826288a4…`) says "Not one of your buddies' devices any
      more… removed on" a date 30 days out (`DATA_DIR/orphans.json`).
- [x] 0.16.0 smoke test (nothing should look different): both start and
      show "Up to date"; a new small file backs up and shows in Browse
      files; "Their pledge to you" unchanged after the restart. Repo
      storage itself is covered by `e2e_repo_files` until senders use it.
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
      across files and resumable restores.
  - [x] **Spike done (2026-10-13), `rustic_core` 0.13.0** (Apache/MIT, the
    restic repository format) with a local backend, 1.9 GB of random data
    (a 1 GB file plus 300 × 3 MB "photos"):

    | Change | Stored by the next backup |
    |---|---|
    | first backup | 1.9 GB |
    | renamed the 1 GB file, moved 300 photos | 0 |
    | overwrote 1 MB mid-file | 1.1–5.7 MB |
    | inserted 1 byte at the start | 0.7–4.4 MB |
    | duplicated a photo | 0 |
    | deleted the photos, forgot old snapshots, pruned | 1.9 → 1.03 GB |

    Restore byte-identical. Peak memory on 2 CPUs: backup ~210–310 MB,
    restore ~250 MB with our own loop (below); on 16 CPUs backup ~560 MB,
    since rustic sizes its threads to the CPU count. Builds next to iroh,
    age and tokio with no conflicts; needs Rust 1.91 (the image uses
    `rust:bookworm`, current). A renamed file is re-read (25 s for 1.9 GB on
    2 CPUs) but not re-sent.
  - **Why `rustic_core` over our own `fastcdc` store:** it already does
    packing (chunks go into ~32 MB packs, not one file each, which matters
    on SMB/NFS), the index, snapshots, forget/prune with safe garbage
    collection, and change detection. Our own store would be
    reimplementing restic. Its storage interface is six calls (list, read,
    read part, create, write, remove of opaque files), so a buddy can be
    the backend through a few new protocol messages.
  - **Don't use rustic's own restore:** it reads on 20 threads with no
    backpressure (1.9 GB peak restoring 1.9 GB) and `.unwrap()`s every
    read, so one failed network read would crash the client. Restore file
    by file with `open_file` / `read_file_at` instead (bounded, our own
    errors and progress).
  - **Privacy gets better:** the buddy stores only encrypted packs, so it
    no longer sees file counts, sizes, folder depth or how many entries
    each folder has (today's 0.14 limits). Site copy and the status page
    change with it.
  - **Keys:** the repo's master key can be built from our own scrypt
    output (a new labelled derivation, like the name key), so the
    passphrase alone still restores everything; no restic key file to
    lose.
  - **Pledge accounting gets simpler:** count the whole repo's size per
    sender (snapshots and old versions included); prune is what frees
    space.
  - **Negotiate per buddy** by protocol version: older buddies keep
    getting whole files. **Opt-in first** (e.g. `CHUNKED_TRANSFERS=on`)
    for a release or two, restores included, then the default.
  - **Decided 2026-10-13:** files already on a buddy are **re-uploaded
    once** into the new format (like 0.14's name change), and the old
    whole-file copies then become 30-day backup copies, so there's one
    format per pairing, not two side by side. Snapshots are kept **every
    one for 24 h, hourly for 2 days, daily for 30 days**, then pruned.
  - **Build order:**
    - [x] 1. Buddy side (client 0.16.0): protocol v5 repo messages
      (list, read part, write, remove of opaque repo files) stored under
      `<buddy-files>/<sender>/.repo/`, each write checked against its
      content-hash name, counted against the pledge, with the disk floor;
      `repo.rs`, e2e test `e2e_repo_files`. Ships ahead of senders so
      buddies are ready when senders start using it.
    - [x] 2. Sender (2026-10-13, `chunked.rs`, `repo_backend.rs`,
      e2e test `e2e_chunked_backup`): rustic backend over the iroh
      connection (reconnects on a drop, never retries a refusal, fails
      fast once the cycle is over, since rustic can return an error while
      its packer threads are still writing), master key from our scrypt
      output (no key file), backups behind `CHUNKED_TRANSFERS=on`, our own
      walker so the exclude rules and folder skipping work as now, idle
      cycles by a scan fingerprint, daily forget (unit-tested policy) +
      prune, whole files for buddies before 0.16.0. Test numbers: renaming
      a 32 MB file and moving 20 photos sent ~2 KB; a 1-byte edit in it
      0.6–5.2 MB (one chunk, 0.5–8 MB, varies with the repo's random
      chunker polynomial).
    - [x] Live on testa → testb, 0.17.0 with `CHUNKED_TRANSFERS=on`
      (2026-10-13): repository created, new files sent in full (52 MB,
      394 MB), a rename sent 2 KB, a copy of an existing file 3.7 KB,
      idle cycles quiet, no warnings; `.repo` 427 MB = the new files, so
      the copy wasn't stored twice. Not yet: a delete.
    - Follow-ups found in phase 2, all done: rustic's threads capped
      at 4 CPUs (0.19.0); reconciliation skipped for chunked buddies
      (0.18.0); Activity lists a chunked cycle's sent and removed files
      (0.20.0), from comparing the new snapshot's tree with its parent's
      (with a freshly loaded index: the one loaded before the backup
      doesn't know the snapshot's own trees).
    - [x] 3. Browse files, restores and History (client 0.18.0,
      `chunked_restore.rs`, in `e2e_chunked_backup`): the latest snapshot
      is the file list; deleted files (and renamed files' old names) come
      from comparing each older snapshot's tree with the latest's,
      skipping unchanged folders; History is a file's distinct earlier
      contents across snapshots; restores read 4 MiB at a time into a
      temp file (not rustic's own restore); "What your buddy sees" lists
      the pack files and peeks one; "Delete permanently" is refused;
      reconciliation is skipped for chunked buddies (the buddy checks
      every repository file's hash as it arrives).
    - [x] Live: browse, restore a file, restore an older version and a
      renamed file's old name on testa from testb. 2026-10-13: browse,
      a restore and "What your buddy sees" checked; its explanation still
      gave the whole-file wording (fixed in 0.18.1). A renamed ISO's old
      name listed as "deleted — recoverable", History offered Version 1,
      and it restored as `.v1.bak` (50 MB, direct, 25.5 MB/s), sha256
      identical to the file. An edited text file's History offered its
      earlier content (0 B before the edit), which restored.
    - [x] 4. Default on in client 0.19.0 (`CHUNKED_TRANSFERS=off` opts
      out; Browse files follows the setting). After the first snapshot of
      a fully readable folder, the whole-file copies sent before are
      deleted on the buddy (kept there 30 days, logged in Activity;
      `retire_whole_files`, in the e2e test). Backups run on at most 4
      CPUs (`CpuCap`, unit-tested). Site copy (About, Getting started,
      Setup, Privacy, which still said buddies see names) and the status
      page say what a buddy sees for each kind.
    - [x] Live on 0.19.0 (2026-10-13: a 2.5 GB chunked backup ran at
      92 MB/s, 277 MiB peak on testa's 2 vCPUs; the buddy card's counts
      still came from whole files and read 0, fixed in 0.19.1): testa's
      old whole-file copies on testb retired
      (Activity line; `ls` shows them under `.versions/`), a big backup's
      memory with the CPU cap, testb (no setting) backing up to testa in
      chunks. Checked 2026-10-13 on 0.19.1: testa's old whole-file
      copies moved to `.versions/n1/` on testb; testa's card reads "10
      files, 2.5 GB in the last snapshot" and testb's "2.3 GB chunked
      backup" (dedup); testb's empty folder backs up to testa as an empty
      chunked snapshot.
- [ ] **A format switch re-uploads everything, over the relay too.**
      0.19.0 made chunked backups the default with no users yet
      (2026-10-13), so every existing pairing re-sent its whole backup
      once, relay traffic included (counted against the free allowance,
      billed on a paid plan). Before the next change like that with real
      users: switch only when a cycle runs direct, or don't count the
      switch's traffic.
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
