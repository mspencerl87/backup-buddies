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

- [ ] Do long transfers still drop? Answers the open question under
      "Known issues". Send something multi-GB with the debug logging on.
      If both run Docker, each still has `172.17.0.1` and the same default
      port, so the docker0 theory can still bite: no drops means it was a
      same-host problem; drops mean filtering bridge addresses is needed.
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

- [ ] **Long transfers drop on a local network.** On the status page as a
      limitation. Seen twice (2026-10-06/07) with testa and testb on the
      same host: minutes into a multi-GB upload, noq's path idle timer
      closes the only IP path (`failed closing path err=LastOpenPath`,
      from `PathTimer::PathIdle`) and the connection times out ~11 s
      later, both times just after testb's mDNS record was re-sighted.
      With debug logging (`RUST_LOG=…,iroh::_events=debug,iroh::socket::remote_map=debug`)
      the selected path was the Docker bridge (`172.17.0.1→172.17.0.1`),
      with IP paths opening and closing every few seconds and a relay path
      present. Likely cause: on 0.0.0.0, iroh offers every interface
      address, `docker0` included even when down, and every Docker host
      has 172.17.0.1 with the same default port, so a buddy's
      172.17.0.1:11235 points back at your own client. Binding to one
      address would fix that but break direct connections when the address
      changes; filtering bridge addresses out of what's offered is the
      likely fix if separate machines (above) don't drop. Cost today: one
      cycle plus a resume (0.8.0), and it's no longer booked as relay
      (0.8.1).

## Backlog

Build when there's a reason to; roughly in order of value.

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
      an interrupted restore still starts over.
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
- [ ] **Chunked, rsync-like transfers. Only if usage shows a need**
      (decided 2026-10-10). Any change re-sends the whole file: a 20 GB VM
      image with one changed block costs 20 GB, billed when it goes over
      the relay. Typical off-site backup files (photos, documents,
      exports) are written once, and live VM disks and databases shouldn't
      be backed up this way anyway. Signal to watch: a few accounts making
      up most of the monthly relay bytes in admin stats, or users asking.
      Rsync itself can't work (the buddy only holds ciphertext);
      the encrypted equivalent (restic/borg/duplicacy) splits files into
      content-defined chunks (`fastcdc`), encrypts each, and sends only new
      ones, with dedup and resumable restores for free. A new storage
      format on the buddy side, so restore, Browse files, versions,
      reconciliation and pledge accounting all change. Start with a
      `rustic_core` spike on a branch, with the buddy as its storage
      backend.
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
