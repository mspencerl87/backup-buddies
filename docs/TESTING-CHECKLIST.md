# Testing checklist

Running list of what's been verified against a live testa/testb pairing,
and what's still open. Update this in place as items get checked off
instead of re-deriving the list from scratch each session.

## Needs testing — direct-connection fix (SYNC_PORT)

While generating the pledge-rejection test data below, noticed every
transfer between testa/testb goes via relay, 100% of the time. Root
cause: `apps/client/docker-compose.yml` never published a UDP port for
iroh's actual sync connection (only the dashboard's TCP port was
published), so neither container could ever receive an inbound
hole-punch attempt — direct connections were structurally impossible,
not just unlucky, for every real deployment of the client as shipped,
not only testa/testb. Fixed by binding iroh to a fixed port
(`SYNC_PORT`, default 11235, configurable) instead of a random one, and
publishing that port as UDP in both `docker-compose.yml` and the
customer-facing snippet in `apps/web/setup.html`. `cargo build`/`test`/
`clippy --all-targets` all clean. Not yet committed/pushed, and not yet
tested against real traffic — testa and testb are both behind the same
host/IP (same NAT), which is itself a hard case for hole-punching (NAT
hairpinning isn't supported by every router/NAT device), so even a fully
correct fix might still show relay-only between these two specifically.
That wouldn't mean the fix is wrong — it'd mean testa/testb isn't a
representative test for this particular thing, since two real customers
are virtually always on two different networks.

- [ ] Decide whether testa/testb is even a reasonable way to verify
      this, given the same-host NAT-hairpin caveat above — may need two
      genuinely different networks (e.g. a phone hotspot vs. home wifi)
      to really confirm direct connections work at all
- [ ] Confirm whether testa/testb's compose builds from source
      (`build: .`) or pulls the published image, to know how to get this
      build onto the test host
- [ ] Once deployed, forward `SYNC_PORT` (11235 by default) on whatever
      router each side is behind, confirm `docker compose logs` shows a
      path other than relay (iroh's `Connection::paths()`/the dashboard's
      "via relay" vs. direct indicator)
- [ ] Confirm a client with no `SYNC_PORT` forwarded still works exactly
      as before (relay-only) — this must be a strict addition, never a
      regression for anyone who doesn't touch their router
- [ ] IPv6 bind is marked optional (`set_is_required(false)`) — confirm
      a container with no IPv6 available still starts cleanly (this is
      the whole reason that override exists; untested against a real
      no-IPv6 host)

## Needs testing — local-network (mDNS) discovery

Added after confirming testa/testb's same-host NAT hairpin makes the
SYNC_PORT fix above untestable there, and the realization that it was
solving the wrong case anyway for same-LAN buddies: two customer NAS
boxes on one home/office network would relay unnecessarily even with
SYNC_PORT working perfectly, forcing every byte through our relay server
(cost + scaling concern) for traffic that never needed to leave the LAN.
Added `iroh-mdns-address-lookup` (same mechanism Syncthing uses for LAN
discovery) via `Endpoint::builder().address_lookup(mdns.clone())` in
`apps/client/src/main.rs`; purely additive — falls back to relay/direct
-over-internet exactly as before when a buddy isn't on the same LAN.
Logs `"found a buddy on the local network (mDNS) — direct LAN connection
possible"` on discovery. `cargo build`/`test`/`clippy --all-targets` all
clean. Not yet tested against real traffic.

- [x] Confirmed on testa/testb: bridge networking genuinely blocks mDNS
      between containers, even same-host. `docker network ls` showed
      `testa_default`/`testb_default` as two fully separate bridge
      networks (each `docker compose` project gets its own by default),
      and `docker compose logs client | grep -i mdns` was empty on both
      sides after a clean restart — no discovery, no error, just silence.
- [x] Decided: switched to `network_mode: host` in both
      `apps/client/docker-compose.yml` and the customer-facing snippet in
      `apps/web/setup.html`, rather than macvlan or leaving mDNS dormant
      — it's the standard way to make mDNS work in Docker, at the cost of
      losing Docker's network isolation (Linux only; Docker Desktop users
      fall back to published ports and lose same-LAN direct connections
      only, documented in both the README and setup.html).
- [x] Found and fixed a real bug this surfaced: `DASHBOARD_PORT` only
      ever controlled Docker's bridge-mode port *mapping* — the app
      itself always bound a hardcoded `0.0.0.0:8080` internally. Under
      `network_mode: host` there's no mapping layer, so two devices on
      one host (testa/testb's exact setup) would've collided on port
      8080 regardless of `DASHBOARD_PORT`. Fixed in
      `apps/client/src/main.rs`: `DASHBOARD_PORT` now drives the actual
      bind address (`DASHBOARD_BIND` still works as a full-address
      override). `cargo build`/`test`/`clippy --all-targets` all clean.
- [x] Applied to testa/testb: both `docker-compose.yml`s updated
      (`network_mode: host`, `ports:` section dropped), `DASHBOARD_PORT`/
      `SYNC_PORT` confirmed still distinct (8080/8081, 11235/11236),
      image pulled, both restarted, both dashboards reachable at their
      usual addresses afterward.
- [x] Confirmed working on testa/testb (2026-10-02): both sides logged
      `"found a buddy on the local network (mDNS)"` for each other, and
      the dashboard buddy card switched from "via relay" to
      **"direct, 0ms"** for a real transfer (7 files, 4.0 GB). This is
      same-host, not genuinely cross-network, but it's real discovery +
      a real connection using the LAN path, not just discovery running
      uselessly alongside relay traffic.
- [x] Log-noise fix verified: swarm-discovery was re-firing
      `DiscoveryEvent::Discovered` for the same already-known peer every
      ~1s, logged at INFO — drowned out everything else within seconds
      of startup on testa/testb. Fixed (log first sighting only, DEBUG
      for repeats) and confirmed clean in a fresh restart's logs — one
      `"found a buddy"` line, no flood, swarm-discovery's own startup
      chatter quieted too (commit `679656f`).
- [ ] Real cross-network test: two clients on an *actual* separate LAN
      from each other (e.g. a machine on home wifi vs. one on a phone
      hotspot, or two genuinely different physical networks) — same-host
      testa/testb can't rule out something that only works because
      they're unusually close (same machine), even though the mDNS
      result above is a good sign. Deferred by Spencer for later.
- [x] Pledge-cap rejection, functionally confirmed (2026-10-02): testb's
      own logs show the real rejection cleanly — `WARN ... rejected file
      — would exceed this buddy's pledge path=pledge-test-5.bin
      attempted_bytes=1074004134 pledged_bytes=5368709120
      received_bytes=4296021318` — correct numbers, correct decision.
      4 files (2, 3, 4, 6 — 4.0 GB) landed under the 5.0 GB cap, the 5th
      was refused.
- [x] **Found and fixed a serious bug this test exposed**: that
      rejection deadlocked the whole connection. testa went completely
      silent for 8+ minutes (0.09% CPU, no crash, no log line) instead of
      logging the failure and moving to the next file. Root cause:
      `handle_put` decides/writes its rejection ack *before* reading the
      incoming body (deliberately, to avoid downloading bytes it's about
      to discard) — but the sender writes its *entire* file body before
      ever trying to read the ack, so the sender's write blocks forever
      on flow-control credit the receiver never grants. Not related to
      today's mDNS/host-networking work — a real, independent,
      pre-existing bug that just hadn't been hit by a real-sized
      rejection before. Fixed with `RecvStream::stop()` (QUIC
      STOP_SENDING) in all four early-rejection branches (file too
      large, invalid path, disk space, pledge exceeded), not just the
      pledge one. `cargo build`/`test`/`clippy --all-targets` clean,
      commit `a9589cf`.
- [x] Deadlock fix confirmed live on testa/testb (2026-10-02): after
      deploying commit `a9589cf` and restarting both, a fresh
      `pledge-test-5.bin` rejection produced a clean, immediate failure
      on testa instead of hanging — `WARN ... failed to back up file —
      will retry next cycle path=pledge-test-5.bin err=failed to send
      file bytes / Caused by: sending stopped by peer: error 0`.
      `RecvStream::stop()` cut off the blocked sender exactly as
      intended; the cycle kept running afterward instead of going
      silent. Pledge-cap rejection test is now fully closed out (both
      the rejection math and the failure-handling path around it).
- [ ] Minor oddity noticed during the re-test, not yet investigated:
      `pledge-test-2`/`4`/`6` got re-sent by testa even though testb
      already had them from before this restart — testa's manifest
      should have remembered them as already-synced. Not a correctness
      issue (same bytes, just redundant work), but worth a look if it
      keeps happening — possibly the manifest not surviving the earlier
      deadlock/restart cleanly. Also noticed a one-off `WARN ... failed
      closing path err=ClosedPath` on testa mid-cycle that didn't stall
      anything (a file landed on testb ~2 minutes later) — likely iroh's
      multipath cleanup (direct LAN path + relay both available now)
      logging noise rather than a real problem, but watch for recurrence.
- [ ] Confirm two buddies on *different* networks still behave exactly
      as before this change (mDNS finds nothing, falls through to
      relay/SYNC_PORT direct-over-internet) — must be a strict addition,
      never a regression. Same underlying need as the cross-network test
      above — one test covers both.
- [ ] Confirm container restart doesn't leave stale mDNS records
      (re-advertise cleanly on startup, old record expires on the other
      side within a reasonable time — `DiscoveryEvent::Expired`)

## Needs testing — dashboard visibility, "Cheap" tier

Spencer asked, after noticing the dashboard gives no indication *why* a
file is stuck failing (only that it is), for a cheap first pass at
Syncthing-style device/transfer visibility — this tier, the harder
Medium/Bigger-lift tiers tracked under "Flagged, not yet scoped" below.
Not yet exercised on testa/testb (built, compiles clean, not yet deployed
+ manually confirmed):

- [x] STOP_SENDING reason codes (`protocol::PutRejectReason` — pledge
      exceeded, too large, invalid path, disk full) now ride the QUIC
      stop code itself, not just the ack string, so the sender learns
      *why* even though it never gets to read the ack (that's the
      deadlock fix from above — the receiver cuts the blocked write
      before the sender could've read a response). `backup.rs` unwraps
      `WriteError::Stopped` via `reject_reason_from_err` and threads the
      human-readable reason through `BackupCycleStats.failed_paths`
      (now `Vec<FailedFile { path, reason }>` instead of bare paths).
      **Confirmed live on testb (2026-10-02)**: the buddy card shows "2
      file(s) won't send/receive: pledge-test-5.bin (your buddy's
      pledge to you is full), pledge-test-1.bin (your buddy's pledge to
      you is full)" — reason text displaying correctly next to each path.
- [ ] New "This Device" card: build version (`CARGO_PKG_VERSION`) and
      process uptime — confirm uptime resets to ~0 right after a
      container restart and climbs normally afterward.
- [ ] Same card: local backup-dir file count + total size (a live
      re-scan via `backup::local_backup_summary`, not anything from the
      manifest) — confirm it shows on a device with a `BACKUP_DIR` set
      and stays hidden (no empty/zero row) on a receive-only device with
      none.
- [x] **Found and fixed while confirming the above**: the same testb
      screenshot showed "stale — last synced never" even though 11
      files / 4.0 GB had clearly synced successfully — `last_ok_at`
      (and therefore staleness) was derived from `ok`, which goes false
      for the whole cycle the moment *any* file fails, even if the
      other 11 keep succeeding every cycle. A buddy with one
      permanently-stuck file looked identical to a buddy where nothing
      had ever synced. Fixed by splitting the two meanings apart:
      `ok`/`last_ok_at`/`consecutive_failures` still drive the "backup
      failed" pill (any failure at all is still worth flagging loudly),
      but staleness and "last synced Xs ago" now come from a new
      `made_progress`/`last_synced_at` pair — true when the cycle sent
      or deleted at least one file, or found nothing that needed doing
      at all (a truly idle, in-sync cycle); false only when every
      attempted put/delete failed or the connection itself failed.
      `cargo build`/`test`/`clippy --all-targets` clean. Not yet
      redeployed/reconfirmed on testa/testb — should show "last synced
      <Ns ago>" instead of "never" on the same testb buddy next refresh
      after deploy.

## Verified working (2026-10-02)

- [x] Backup cycle: testa → testb, new/changed files detected and sent
      every ~30s
- [x] Deletion mirroring: deleting a file locally on the sender removes
      it from the buddy's live copy
- [x] Dashboard disk-summary display (was reverting to "checking…" on
      every refresh — fixed, confirmed via Playwright + manual check)
- [x] Deleted files staying visible in the buddy file browser instead of
      vanishing (greyed out, History-only, no plain Restore) — fixed in
      commit `898aaf3`
- [x] Per-version restore ("Restore this version" on a deleted file) —
      was hanging forever (missing `send.finish()` on the response
      stream), fixed in commit `766e510`, confirmed with a real restore
      completing (182 B in 2.6s via relay)
- [x] `setup.html` documents where restored files land
      (`data/restored/<buddy>/`) — fixed in commit `a771d4c`
- [x] Plain "Restore" on a single live (non-deleted) file — confirmed
      (`bigfile.bin`, 2.0 MB in 3.5s via relay)
- [x] "Restore all" on a buddy with a mix of live files — confirmed
      (7 files, 2.0 MB in 17.6s via relay)
- [x] Live restore progress + ETA (file N of M, bytes so far, ~time
      remaining) instead of one frozen "Restoring…" — added in commit
      `d7cf342`, confirmed showing live updates during a restore
- [x] Free-tier accounts can actually create a pledge/pairing — was
      impossible (`PLEDGE_STEP_BYTES` floor of 500GB vs. `FREE_GB` of 50
      meant the smallest legal pledge was already 10x the entire free
      allowance). Lowered the step to 5GB in commit `3b76eab`; confirmed
      deployed (`deploy.yml` run succeeded against `3b76eab`).
- [x] Exact-size marketplace matching shipped (commit `4910d6f`, deployed
      — `deploy.yml` run `37053290026` succeeded): `GET /invites/public`
      requires `pledged_bytes` and only returns that exact size;
      `POST /invites/public/:id/redeem` requires it too and 409s on a
      mismatch. Still not manually exercised end-to-end on testa/testb —
      see below.

## Needs testing (new — not yet exercised on testa/testb)

From the exact-size matching fix (commit `4910d6f`, deployed):

- [ ] Picking a different size on "Find a buddy" actually re-queries and
      swaps the listed buddies, including the now-you-see-it/now-you-
      don't case of a size with zero listings (the empty-state message)
- [ ] Redeeming a listing sends the selected size back to the API
      (`pledged_bytes` in the POST body) and the resulting pairing's
      `pledged_bytes` matches exactly — confirm end-to-end on
      testa/testb, not just by reading the code
- [ ] `GET /invites/public` without a `pledged_bytes` query param returns
      400 — confirm a direct/manual API call gets the expected rejection
- [ ] The redeem-time mismatch guard (`expectedPledgedBytes` check in
      `redeemInvite`) actually fires on a 409 if forced — this needs a
      deliberate test (e.g. two browser tabs, or a raw `curl` with a
      `pledged_bytes` that doesn't match the listing id) since the normal
      UI flow can't produce a mismatch on its own

From the listing-criteria + full-page browse fix (not yet committed as of
this writing — see "Marketplace listing criteria" below):

- [x] Migration `010_marketplace_listing_criteria.sql` applied to
      production (4x `ALTER TABLE` + `CREATE INDEX`, no errors) — commit
      `6c2cab4`/deploy `37054888082` both confirmed clean
- [ ] Creating an invite with country/speed tier/always-on/relay-only set
      actually stores them — check "Invites you've sent" or query the DB
      row directly, since that table doesn't display the new columns
- [ ] Those same fields are only sent to the API when "List this
      publicly" is checked (`invite-criteria-rows` toggle) — confirm an
      unchecked private invite doesn't submit stale values from a
      previously-checked session
- [ ] `find-a-buddy.html` loads standalone (not just as a tab inside
      dashboard.html) — confirm the shared `api-utils.js`/
      `pledge-options.js` scripts actually load given Caddy's static root
      is `apps/web` (should be automatic, same directory, but confirm
      post-deploy since this is a brand new page)
- [ ] Pagination: `?limit=`/`?offset=` and `has_more`/`total_count` — hard
      to exercise for real without 50+ listings at one size; at minimum
      confirm "Load more" stays hidden when there's only a handful
- [ ] Landing on `find-a-buddy.html?pledged_bytes=<bytes>` from the
      dashboard's "Browse buddies" link pre-selects the right size in the
      dropdown
- [ ] The `pledged-gb` (dashboard, create) and `size-filter`
      (find-a-buddy.html, browse) dropdowns stay in sync after any future
      `PLEDGE_STEP_BYTES`/`PLEDGE_MAX_BYTES` change — both now generate
      from the single `PLEDGE_GB_OPTIONS` list in `apps/web/pledge-
      options.js`, but that list is hand-maintained and not pulled from
      the API, so a future API-side step/max change needs the same edit
      made there too

## Not yet exercised

Restore path (single file, all files, per-version restore of a deleted
file) is now fully exercised. Remaining:

- [ ] Pledge-enforcement rejection: a buddy sending more than they're
      pledged should be rejected — never triggered
- [ ] Disk-space-floor rejection (2GB flat / 5% of filesystem,
      whichever's bigger) — never triggered
- [ ] Stale-buddy badge (`STALE_AFTER_SECS`, default 10 min) — never
      triggered
- [ ] Relay-bandwidth billing report (the ~30s "how many MB since last
      report" cycle) — never specifically checked

## Housekeeping (not bugs)

- [ ] Pledges aren't set via `.env` at all — they're created through the
      website's invite/redeem flow (`buddy_pairings.pledged_bytes`,
      fixed once a pairing is created, no update route). To actually
      exercise the disk-space-floor / pledge-enforcement rejection paths
      on testa/testb: delete the existing pairing from the website and
      re-invite at a small size (now possible — 5GB and up — instead of
      the old 500GB floor) that's realistic against the ~47.9GB real
      disk (under ~37GB free, say 30-35GB)
- [ ] `apps/api/src/routes/stripe-webhook.js` — open product decision
      (not a bug): on `invoice.payment_failed`, relay access is paused
      immediately. TODO in that file asks whether a grace period should
      come first.

## Design note: marketplace size-matching

"Find a buddy" used to show every public listing (grouped into
expandable size tiers just for readability). That meant someone who only
needed 5GB could end up redeeming a 500GB-20TB listing — `pledged_bytes`
is shared by both sides of the resulting pairing, so that's not a
convenience, it's an unwanted obligation on both ends. Fixed by always
filtering to one exact size:

- `GET /invites/public` now requires a `pledged_bytes` query param and
  only returns listings at that exact size (400 if missing/invalid)
- The dashboard's "Find a buddy" card has its own "Show listings
  pledging" dropdown (defaults to 50GB) instead of browsing everything
- `POST /invites/public/:id/redeem` now requires `pledged_bytes` in the
  body and rejects (409) if it doesn't match the listing's actual size —
  a server-side belt-and-suspenders check in case the dashboard's filter
  is ever stale, not just a client-side convenience
- Direct code-based redeem (`/invites/:code/redeem`) is unchanged — two
  people already agreed on a size out of band when one sent the other a
  code, so there's nothing to match there

Committed and deployed as `4910d6f` — see "Verified working" above.

## Design note: marketplace listing criteria (country, speed, etc.)

Raised while discussing the size-matching fix above: exact pledge size
alone doesn't rule out a US-based buddy matching with one in a country with
terrible mutual latency, or someone on gigabit fiber pairing with someone
on a 2 Mbps uplink. Added four optional, self-reported fields to invite
creation, shown on marketplace listings but — unlike pledged_bytes — never
used to filter what's returned, so browsing still shows everyone at a
given size and lets the person judge fit themselves:

- **Country** (dropdown, `invites.country`) — country only, deliberately;
  no state/city/postal code, consistent with the marketplace's existing
  "nothing here reveals who someone is" stance on identity.
- **Upload speed tier** (`invites.speed_tier`, one of `under_10` /
  `10_50` / `50_200` / `200_plus`) — bucketed rather than a typed Mbps
  number, since a raw figure is rarely accurate anyway (varies by time of
  day, ISP throttling, Wi-Fi vs. wired).
- **Usually online** (`invites.always_on`, boolean) — self-reported; not
  derived from `devices.last_seen_at` for this first pass.
- **Relay-only network** (`invites.relay_only`, boolean) — self-reported.
  Auto-detecting this would need the client to report direct-vs-relay
  ratio back to the API (it already knows this per-transfer, shown in the
  local dashboard's "via relay, 92ms" messages, but never reports it
  centrally) — real plumbing work, deferred; see "Flagged, not yet
  scoped" below.

Also added, but *derived* rather than self-reported — no new columns:
**member since** (`accounts.created_at`) and **completed pairings**
(count of that account's `buddy_pairings` with status `active` or
`revoked`), as a lightweight trust signal, not a rating system.

**"Find a buddy" became its own page** (`apps/web/find-a-buddy.html`)
rather than an inline dashboard card, since a marketplace showing all this
per listing — plus however many listings a size tier grows to — doesn't
fit in a card. The dashboard's card is now a one-line teaser + "Browse
buddies" link (carries the dashboard's currently-selected pledge size
across as `?pledged_bytes=`); the new page owns its own size filter,
paginates listings (`?limit=`/`?offset=`, default page 50, max 100), and
redeems the same way the old inline card did. Shared JS
(`api-utils.js`, `pledge-options.js`) factored out so both pages use the
same `parseJsonResponse`/`loadMe`/`showBlockedError` helpers and the same
pledge-size list, rather than copy-pasting it into a second page.

**Deploy order — read before pushing this one:** this adds four columns
to `invites` (migration `010_marketplace_listing_criteria.sql`).
`deploy.yml` never runs migrations — it only pulls and restarts the API
image — so if the code deploys before the migration is applied, every
`GET`/`POST /invites*` call 500s until someone runs the migration by
hand. All four new columns are additive with defaults/nullable, so
there's no reason to sequence it tightly: **apply the migration against
production whenever convenient, any time before or after this code
ships** — old API code never references the new columns, so running it
early is harmless, and that removes the race entirely. See
`docs/DEPLOY.md` for the exact `psql` command (same pattern as every
other migration there).

## Documented, intentional limits (no action needed unless priorities change)

- Backup cycle is a fixed ~30s, not configurable
- No per-file/pattern exclude list for what gets backed up
- Local status dashboard has no login of its own

## Flagged, not yet scoped

- [ ] Restore progress is now live, but the modal itself still has to
      stay open to see it (closing it doesn't cancel the restore,
      which keeps running server-side, but you lose the progress view
      and have to reopen "Files from <buddy>" to pick it back up — the
      progress endpoint would still answer correctly if it did).
      Worth a small persistent indicator elsewhere on the dashboard
      (e.g. on the buddy's card) if that's annoying in practice.
- [ ] `always_on`/`relay_only` marketplace listing fields are self-
      reported for now (see "Design note: marketplace listing criteria"
      above). Auto-deriving them from real telemetry would need: (a) a
      new table/column for the client to report direct-vs-relay ratio
      per transfer (it already knows this locally, just never sends it),
      and (b) `devices.last_seen_at` history turned into an actual uptime
      percentage rather than a point-in-time timestamp. Worth it only if
      self-reports turn out unreliable in practice.
- [ ] **Reconciliation against the buddy's actual state, not just the
      local manifest.** Raised by Spencer after manually `rm -r`'ing
      files on testb's disk (bypassing the protocol) and the sender
      never noticing — do after the current deadlock-fix re-test is
      clean. Right now `run_backup_cycle` only ever diffs local files
      against its own manifest (what it *believes* it already sent); a
      failed `put` does retry automatically next cycle (manifest isn't
      updated on failure, so it naturally stays in the "changed" diff —
      that part already works, confirmed by the deadlock-fix finding
      above), but if the manifest and the buddy's actual stored files
      ever drift for any reason *outside* that normal put/delete flow —
      manual filesystem surgery on the receiver, a bug, partial data
      loss on their end, restoring from an old snapshot — the sender has
      no way to detect it. Nothing about the local source file changed,
      so it's never re-evaluated. Proposed fix: periodically (not every
      30s cycle — this is heavier) call the buddy's existing `List`
      request (already used by restore/browse) and diff the *response*
      against the manifest too, not just local-vs-manifest. Any mismatch
      (buddy's missing something the manifest thinks landed, or has
      something stale) triggers a re-send or at minimum a loud,
      visible warning rather than silent, permanent drift.
- [ ] **Syncthing-style dashboard visibility — Medium and Bigger-lift
      tiers.** Spencer shared a screenshot of Syncthing's Folders/"This
      Device" panels as a template and asked for a cheap tier now, the
      rest later (the cheap tier — STOP_SENDING reason codes surfaced on
      the dashboard, plus version/uptime/local-file-count — shipped
      below). Remaining, roughly in order of how much they'd cost:
  - Medium:
    - Global State rollup — Syncthing's Folders panel shows "Global
      State" (what the whole cluster has) vs. "Local State" (what this
      node has). Our closest equivalent would be, per buddy: our
      manifest's file count/bytes (what we believe we've sent) vs. their
      actual received/ count/bytes (available already via the existing
      `List`/`DiskStats` requests) — i.e. a live reconciliation check,
      which overlaps a lot with the reconciliation backlog item above;
      probably worth doing as one piece of work rather than two.
    - Rescan interval/toggle display — we don't have a configurable scan
      interval today (fixed ~30s cycle, see "Documented, intentional
      limits" above), so this would first need that to become
      configurable before there's anything to display.
    - Discovery peer count — how many buddies are currently visible via
      mDNS on the local network right now, not just logged once at
      discovery time. Needs the main.rs mDNS task to publish a live
      count somewhere the dashboard can read (e.g. a shared
      `Arc<Mutex<HashSet<EndpointId>>>` alongside the existing dedup set)
      rather than only logging.
  - Bigger lift:
    - Live streaming transfer rate (bytes/sec updating during an
      in-progress cycle, not just the after-the-fact rate computed from
      a finished cycle's bytes_sent/duration_ms). Needs the backup loop
      to report progress incrementally rather than only at cycle end —
      similar shape to the restore-progress work already done, but for
      the outbound/backup path.
    - Listener reachability checks — Syncthing actively verifies each
      configured listen address is actually reachable from outside.
      We have no equivalent concept; would need an active probe (self-
      connect through the relay, or ask a buddy to connect back) rather
      than just reporting the configured SYNC_PORT.
    - Multi-buddy "Shared With" — Syncthing's model is one folder shared
      with N devices; ours is 1:1 (one buddy relationship = one pledge,
      one manifest). Showing "shared with" as a list doesn't map onto
      the current architecture at all — this would be a real design
      change (a backup_dir shared with multiple buddies at once), not a
      dashboard addition, so it stays out of scope here.
