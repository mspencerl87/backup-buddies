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

- [x] **Relay fallback on a real, locked-down network — confirmed working
      (2026-10-03).** testc runs on a genuinely separate network from
      testa/testb (`msl-ubuntu`, Spencer's company lab — no access to that
      firewall, port-forwarding not possible there), making this the
      first real (non-same-host) test of this whole feature. Triggered a
      real file send from testb to testc: `sent 1, deleted 0 · 10.5 KB in
      2.8s (3.8 KB/s) · via relay, 133ms` — confirmed by the independent
      "Their actual disk" probe connection on the same card also landing
      on `via relay, 131ms`. Hole-punching didn't succeed for this pairing,
      but the fallback did exactly what it's designed to do: the transfer
      completed correctly over relay, no data loss, no error. This *is*
      the product working as intended on a network a user doesn't
      control — see the "why relay, and is that fixable" note below for
      why this isn't a bug.
- [ ] **Direct connection when forwarding is actually possible — still
      unconfirmed.** Needs two networks Spencer (or whoever's testing)
      actually controls on at least one, ideally both, sides (e.g. a
      phone hotspot vs. home wifi) so `SYNC_PORT` (11235/udp) can really
      be forwarded and hole-punching gets a fair shot. testa/testb (same
      host) and testb/testc (company network, no firewall access) have
      both now been ruled out as ways to confirm this specifically.
- [ ] Confirm a client with no `SYNC_PORT` forwarded still works exactly
      as before (relay-only) — already effectively demonstrated by the
      testc result above, but worth a clean confirmation once the direct
      case above is also confirmed, so both ends of the behavior are on
      record together
- [ ] IPv6 bind is marked optional (`set_is_required(false)`) — confirm
      a container with no IPv6 available still starts cleanly (this is
      the whole reason that override exists; untested against a real
      no-IPv6 host)

**Why relay, and is that fixable? (2026-10-03)** Spencer asked whether we
could avoid the port-forwarding requirement entirely — e.g. some
Syncthing-style scheme where the relay only brokers an introduction and
never carries data. That's already exactly how this works: iroh (the P2P
library backing this app) always attempts a direct, hole-punched UDP path
first, using the relay purely to let each side learn the other's
reachable address; only if that attempt fails does the relay keep
carrying the QUIC stream as a fallback (see `netinfo.rs`'s doc comment).
So there's no missing handshake step to build — the one that ran for
testc's transfer already tried direct first and fell back correctly.

The actual reason hole-punching failed here, forwarded port or not: it
depends on both sides' NAT preserving the same external port across
connection attempts (full-cone / restricted-cone NAT) so each side can
correctly guess the other's address. Corporate networks very commonly use
**symmetric NAT**, which allocates a new external port per connection —
this makes blind hole-punching fail deterministically, independent of
whether `SYNC_PORT` is forwarded. This is a hard limit of P2P networking
over UDP, not something client-side code can route around (the same limit
Syncthing, Tailscale, etc. all hit on networks like this). The only way
past it without forwarding is running infrastructure we don't have today
(a TURN-like rendezvous point) — flagged as a possible future scope item,
not a near-term fix.

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
- [x] **Investigated and fixed** the redundant re-send oddity
      (`pledge-test-2`/`4`/`6` got re-sent by testa even though testb
      already had them). Root cause found by reading `backup.rs`:
      `manifest.save()` was only called **once, after the entire
      cycle's loop finished** — every successful put/delete only
      updated the in-memory `Manifest`, not disk, until the whole
      cycle was done. If the process gets killed partway through a
      cycle (a manual restart, an OOM, or — exactly what happened here
      — a later file in the same cycle deadlocking the connection
      before the STOP_SENDING fix existed, which the user worked
      around by restarting the container), every file that had already
      landed successfully earlier in *that same cycle* never got
      persisted, so the next cycle saw them as "changed" again and
      re-sent them. Not a correctness bug (the buddy's put handling is
      idempotent — same bytes land again, nothing breaks), just wasted
      bandwidth/time, which matches exactly what was observed. Fixed in
      `backup.rs`: `manifest.save()` now runs after *each* successful
      put/delete, not just once at the end (a non-fatal `tracing::warn`
      if an individual save fails, since losing one isn't data loss —
      just a possible future redundant re-send of that one file). Also
      hardened `manifest.rs` while in there: `save()` now writes to a
      temp file and renames over the real path (atomic on the same
      filesystem) instead of truncate-and-write-in-place, so a process
      killed mid-write can't leave a corrupted/truncated manifest
      behind; `load()` now logs a warning (rather than silently
      defaulting to empty) when a manifest file exists but fails to
      parse, so real corruption is visible instead of looking like a
      brand-new buddy. `cargo build`/`test`/`clippy --all-targets`
      clean. Not yet redeployed/reconfirmed on testa/testb.
- [x] Separately, a one-off `WARN ... failed closing path err=ClosedPath`
      on testa mid-cycle that didn't stall anything — **investigated,
      confirmed benign, understood root cause** (2026-10-03). Traced
      through the vendored `iroh`/`noq-proto` 1.3.0 source: it's a race
      between two pieces of *iroh's own* internal multipath logic, not
      anything in our code. A per-path idle timer periodically closes a
      path that's gone quiet, while separately iroh's "redundant direct
      path cleanup" logic (`apply_selected_path` in
      `remote_state.rs`) actively closes extra IP paths once multipath
      selection settles on a winner — exactly what happens right after
      mDNS gives a connection both a LAN path and a relay path and iroh
      picks one. Whichever of the two actually closes the path first
      wins; the other hits `ClosePathError::ClosedPath` ("already
      closed, nothing to do") and logs a `WARN` purely cosmetically —
      the path ends up correctly closed either way, nothing is left
      half-done. No fix on our side exists short of globally silencing
      that whole dependency's WARN-level logs (the way `swarm_discovery`
      is quieted in `main.rs`'s `EnvFilter`), which isn't worth it for
      one harmless, rare, non-stalling race. Closing this out as
      understood rather than an open mystery; no code change made.
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
- [x] New "This Device" card: build version (`CARGO_PKG_VERSION`) and
      process uptime. **Confirmed live on testa (2026-10-03)**: shows
      "Version 0.1.0, Uptime 2m" right after a fresh restart — climbing
      from near-zero as expected.
- [x] Same card: local backup-dir file count + total size (a live
      re-scan via `backup::local_backup_summary`, not anything from the
      manifest). **Confirmed live on testa (2026-10-03)**: "Local files
      13, Local size 6.0 GB", matching testa's real `BACKUP_DIR`
      contents. Still worth a glance on a receive-only device sometime
      to confirm the two stats stay hidden there, but not blocking —
      the code path is a simple `Option` check.
- [x] **Found and fixed while confirming the above**: the same testb
      screenshot showed "stale — last synced never" even though 11
      files / 4.0 GB had clearly synced successfully — `last_ok_at`
      (and therefore staleness) was derived from `ok`, which goes false
      for the whole cycle the moment *any* file fails, even if the
      other 11 keep succeeding every cycle. A buddy with one
      permanently-stuck file looked identical to a buddy where nothing
      had ever synced. First attempt split `ok` from a new
      `made_progress` flag but still tied `made_progress` to
      `files_failed == 0` for the "nothing new to send" cycle shape —
      which doesn't actually fix this case, since `pledge-test-5.bin`/
      `pledge-test-1.bin` are *permanently* stuck (always in the diff,
      always fail) and the 11 good files are already fully synced and
      never re-enter the diff, so under that version this specific
      buddy would show "stale — last synced never" forever, no matter
      how long the deploy runs. Corrected: `made_progress` now means
      "the connection itself worked and a cycle actually ran" — true
      any time we successfully connect and complete a cycle, *whether
      or not* individual files are rejected, and false only when the
      connection itself fails. The specific-file problem stays loudly
      visible via the separate red failed-files line and the "backup
      failed" pill right next to it; staleness now answers "can we
      reach this buddy at all" instead of restating that. `cargo
      build`/`test`/`clippy --all-targets` clean. **Confirmed live on
      testb (2026-10-02)**: same buddy, same 2 permanently-pledge-
      exceeded files — stale pill is gone entirely, only "backup
      failed" remains, and both per-file reason lines still show
      correctly. Staleness now correctly reflects reachability, not
      per-file rejects.

## Needs testing — reconciliation against the buddy's actual state

Raised by Spencer after manually `rm -r`'ing files on testb's disk
(bypassing the protocol) and the sender never noticing — moved from
"Flagged, not yet scoped" to here now that it's built (2026-10-03). Right
now `run_backup_cycle` only ever diffs local files against its own
manifest (what it *believes* it already sent) — a failed `put` does retry
automatically next cycle (manifest isn't updated on failure, so it
naturally stays in the "changed" diff — already confirmed working by the
deadlock-fix finding earlier), but if the manifest and the buddy's actual
stored files ever drift for any reason *outside* that normal put/delete
flow — manual filesystem surgery on the receiver, a bug, partial data
loss on their end, restoring from an old snapshot — nothing about the
local source file changed, so it was never re-evaluated and the sender
had no way to detect it.

Implemented as `backup::reconcile_with_buddy` in `backup.rs`, called
periodically (every 10 minutes per buddy, independent of the ordinary
~30s cycle — see `RECONCILE_INTERVAL` in `main.rs`'s outbound loop, not
gated on whether that cycle had anything to send, since the whole point
is to catch drift that *never* shows up as a local change) rather than
every cycle, since it costs the buddy a full `List` of everything they're
holding for us — heavier than the ordinary diff-driven cycle, which only
touches the network when something local actually changed:

- Compares every manifest entry against the buddy's live `List` response
  (a `deleted: true` entry — a `.versions/`-only backup slot — counts as
  "buddy doesn't have this live", same as not being listed at all) by
  both presence and ciphertext size.
- Anything the manifest believes landed but doesn't actually match on the
  buddy's side gets dropped from the manifest right there — no separate
  re-send logic needed, the very next ordinary cycle sees that local file
  as "changed" (no longer in the manifest) and re-sends it automatically
  through the existing, already-tested path.
- Anything the buddy has live that our manifest has no record of is only
  *reported*, never auto-deleted from the buddy's side — guessing wrong
  and destroying a buddy's data over a stale local manifest would be far
  worse than a loud warning asking a person to look into it.
- Logs at `WARN` with both path lists when anything's found, `DEBUG` when
  clean.
- **Now also surfaced on the dashboard** (2026-10-03, same session as the
  core feature — the Medium-tier "Global State rollup" deferral note
  turned out to be a small enough addition to just do immediately rather
  than leave as a second follow-up): `dashboard::ReconcileRecord` /
  `ReconcileBook` (new, mirrors `CycleRecord`/`CycleBook`'s shape) is
  updated by the same outbound-loop reconciliation call and read by
  `status_handler` into a new `BuddyStatus.last_reconcile` field. The
  buddy card shows a line below the failed-files warning: "✓ reconciled
  Xm ago — N file(s) match the buddy's actual records" when clean,
  "⚠ reconciliation (Xm ago) found drift — …" listing both path lists
  when not, or "⚠ reconciliation check failed Xm ago: …" if the
  reconciliation attempt itself errored (couldn't connect/list) — kept
  distinct from "checked and found drift" so one doesn't masquerade as
  the other. No line at all for a buddy whose manifest is still empty
  (nothing to reconcile yet) rather than a confusing "0 files match".

`cargo build`/`test`/`clippy --all-targets` clean.

- [x] Confirmed the dashboard line renders correctly (2026-10-03):
      testb showed "✓ reconciled 9m ago — 11 file(s) match the buddy's
      actual records" on a clean pairing before any deliberate drift was
      introduced — same pass also covers the "no false positives on a
      healthy pairing" item below.
- [x] Confirmed the periodic tick actually fires and finds real drift
      (2026-10-03): manually deleted `pledge-test-3.bin` from testb's
      `received/<testa>/` dir, and testa's very next reconciliation tick
      logged `WARN ... reconciliation found drift ... checked=10
      missing_on_buddy=1 missing_paths=["pledge-test-3.bin"]
      unexpected_on_buddy=0 unexpected_paths=[]` — exact path, clean
      unexpected-side too (nothing spuriously flagged).
- [x] **The actual scenario that prompted this — fully closed out
      (2026-10-03)**: after the `WARN` above, testa retried
      `pledge-test-3.bin` every ordinary cycle exactly like any other
      pending file (not a special reconciliation-only path) — correctly
      blocked each time by the pre-existing pledge cap
      (`sending stopped by peer: error 1 reason="your buddy's pledge to
      you is full"`, same reason code as `pledge-test-1`/`-5`, not a new
      failure mode). Once pledge headroom was freed (deleted an already-
      landed file to make room), the very next cycle succeeded: testb
      logged `INFO ... stored file from buddy ... path=pledge-test-3.bin
      bytes=1074004134`. Detection → retry-under-constraint → successful
      recovery, all confirmed end to end.
- [x] No false positives on a healthy pairing — covered by the clean
      "✓ reconciled" dashboard line above, seen repeatedly across several
      ordinary cycles with no spurious drift reported.
- [x] Confirmed the *unexpected*-file case (2026-10-03): manually dropped
      `unexpected-test.bin` directly into testb's
      `received/<testa>/` dir (bypassing the protocol, same as the
      missing-file test but in reverse). testa's next reconciliation tick
      logged `WARN ... missing_on_buddy=0 missing_paths=[]
      unexpected_on_buddy=1 unexpected_paths=["unexpected-test.bin"]` —
      correctly isolated from the missing-file case. Confirmed via `ls`
      on testb afterward that the file was left untouched — reconciliation
      only reports unexpected files, never deletes them.
- [x] Confirmed a receive-only device never reconciles (2026-10-03):
      testc (new this session, no `BACKUP_DIR`, empty manifest, paired
      with testb) shows no reconciliation line at all on its testb buddy
      card — neither a clean "✓ reconciled" nor a drift warning, just the
      ordinary backup/pledge/disk info. Matches the documented "no line
      for a buddy whose manifest is still empty" behavior exactly.

Reconciliation feature: every checklist item above now confirmed. Closing
this section out.

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

## Verified working (2026-10-03)

- [x] **Found and fixed: duplicate active buddy pairings.** Nothing
      stopped two accounts from ending up with two active
      `buddy_pairings` rows between them (each side invites the other,
      both get redeemed) — caught live on testa/testb's dashboard as two
      identical-looking 5GB rows for the same buddy. Fixed in commit
      `8ed4f4d`: `redeemInvite` now 409s on a second invite between an
      already-paired pair; migration `011_prevent_duplicate_pairings.sql`
      dedupes existing doubles (keeps the oldest active row, revokes the
      rest) and adds a unique index on the normalized pair (scoped to
      `status = 'active'`) that closes the race a check-then-insert
      can't. Verified against a real local Postgres before deploying
      (migrations apply cleanly in sequence, dedup keeps the right row,
      the index rejects a fresh duplicate, re-pairing after a revoke
      still works) and confirmed live on prod: testb's billing total
      correctly dropped from 60GB (double-counted) to 55GB, and the
      duplicate pairing row showed `revoked` instead of a second
      `active`.
- [x] **Hide revoked pairings from the dashboard** (commit `1a44f61`):
      a revoked pairing has nothing left to do with it (no reactivate
      flow), so `GET /me` now filters to `status = 'active'` for the
      dashboard's pairings table. Deliberately not deleted — the
      marketplace's "completed pairings" trust signal
      (`GET /invites/public`) counts revoked pairings too. Confirmed live
      on testb: the `revoked` row from the dedup above disappeared from
      the dashboard after redeploy.
- [x] **Found and fixed: CI deploy vs. manual deploy race.**
      `deploy.yml` already auto-deploys any push touching `apps/api/**`/
      `deploy/app/**`/`db/migrations/**`, but `docs/DEPLOY.md`'s
      migration instructions (going back to section 9) also told you to
      manually `docker compose up -d --build api` right after — pushing
      the duplicate-pairings commit above auto-triggered CI's own deploy
      at the same moment that instruction had someone running the
      identical command by hand over SSH, and both `docker compose up`
      invocations raced for the `app-api-1` container name (one failed
      outright: "Error when allocating new name: Conflict", visible both
      in the failed Actions run for that commit and live in the
      terminal). No lasting damage — confirmed `app-api-1` ended up
      healthy on the correct image either way — but fixed the actual
      instructions in commits `f253958`/`2754371`: sections that only
      ever needed the restart for code already covered by CI (§11, §15's
      migration step) dropped the manual redeploy entirely; sections that
      restart for a genuinely new env var (§9, §10, §12, §13, §14, §15's
      enable step) kept it but dropped the redundant local `--build` and
      now say to let CI's own deploy finish first instead of racing it.
- [x] **Found and fixed: the stale-buddy badge never tripped for a
      genuinely unreachable buddy.** Surfaced by deliberately downing
      testc and watching testb's dashboard — after 10+ minutes it still
      showed testc as "backup ok", even though reconciliation was
      logging repeated connection timeouts to it in the same logs.
      Root cause: the ordinary ~30s backup cycle's "nothing new to send"
      path never opens a connection at all (per `run_backup_cycle`'s own
      doc comment), but the dashboard code handling that case
      unconditionally set `made_progress: true` with a comment claiming
      "we successfully connected and ran a cycle" — reachability was
      assumed, never actually checked, so a dead buddy with nothing new
      to resend could sit at "backup ok" indefinitely. Fixed in commit
      `b5d3b98` (`apps/client/src/main.rs`): that idle case
      (`files_sent == 0 && files_deleted == 0 && files_failed == 0`) now
      runs the existing `Ping` protocol check (previously only used for
      receive-only buddies with no `BACKUP_DIR`) and bases `ok`/
      `made_progress` on whether the ping actually succeeds. The
      permanently-stuck-file case (`files_failed > 0`) is unchanged,
      since `run_backup_cycle` did open a connection there already.
      `cargo build`/`test`/`clippy --all-targets` clean. **Confirmed live
      on testb (2026-10-03)** after rebuilding/redeploying the client:
      with testc still down, testb's log now reads `WARN ... nothing new
      to back up, and connectivity check failed buddy=testc
      detail=nothing new to back up, but connectivity check failed:
      failed to connect to buddy over Iroh`, and the dashboard card shows
      both `backup failed` and `stale — last synced 11m ago` badges
      correctly tripped, agreeing with reconciliation's own independent
      `reconciliation check failed` line for the same buddy.

## Needs testing (new — not yet exercised on testa/testb)

**Partial prior confirmation (predates this list, recalled 2026-10-03):**
Spencer tested the underlying find-a-buddy mechanism early in this
project's testing, before the items below existed — generated fresh
testa/testb device tokens and, instead of using a direct invite code,
used the public "list publicly"/find-a-buddy browse flow, which correctly
showed a single listing at the then-current pledge size (500GB, before
`PLEDGE_STEP_BYTES` was lowered to 5GB). That's real confirmation the
basic idea works — browsing by size, instead of a code, finds and shows
the right listing. It predates (so doesn't cover) the exact-size
redeem-time validation (`4910d6f`), `find-a-buddy.html` becoming its own
standalone page, and the listing-criteria fields below — all shipped
later. Narrowing the list below accordingly rather than re-testing
everything from scratch.

From the exact-size matching fix (commit `4910d6f`, deployed):

- [x] **Confirmed live end-to-end (2026-10-03)**, using two fresh
      throwaway accounts (not testa/testb/testc, to avoid disturbing
      those pairings) rather than subscribing just to free up pledge
      room: account A created both a 20GB and a 15GB public listing;
      account B's "Find a buddy" page correctly showed only the matching
      listing at each size filter, correctly showed the empty state at a
      size with nothing listed (30GB), and correctly swapped results when
      the filter was changed between sizes — re-querying works, not just
      the initial load.
- [x] **Confirmed live (2026-10-03)**: redeemed the 15GB listing from
      account B; account B's own dashboard pairing table shows `15 GB,
      active` — the exact size redeemed, not 20GB or any other value.
      `pledged_bytes` really does flow end-to-end through redeem, not
      just validated by reading the code.
- [x] **Confirmed live (2026-10-03)**: `curl` against `GET
      /invites/public` with a valid session cookie but no `pledged_bytes`
      param returned a clean `400 {"error":"pledged_bytes query parameter
      is required"}`.
- [x] **Confirmed live (2026-10-03), with a real finding along the way**:
      first attempt (redeeming the 20GB listing again from the account
      that had already redeemed the 15GB one, with a wrong
      `pledged_bytes`) returned `409 "you and this account are already
      paired up"` — correct response, but the *wrong* guard: redeemInvite
      checks for an existing active pairing (line ~190) before it ever
      reaches the size-mismatch check (line ~214), so that first test
      never actually exercised the mismatch logic. Re-ran with a third,
      unpaired throwaway account redeeming the same 20GB listing with
      `pledged_bytes: 999999999` and got the real thing: `409 "this
      listing pledges 20GB, not 1GB — refresh \"Find a buddy\" and try
      again"`. Both guards now independently confirmed working, each
      firing in its own right scenario, not conflated.

Marketplace exact-size matching + listing criteria: every item above now
confirmed except the dashboard-link size-preselect and the two dropdowns'
long-term sync, both low-risk/low-priority — see below.

From the listing-criteria + full-page browse fix (not yet committed as of
this writing — see "Marketplace listing criteria" below):

- [x] Migration `010_marketplace_listing_criteria.sql` applied to
      production (4x `ALTER TABLE` + `CREATE INDEX`, no errors) — commit
      `6c2cab4`/deploy `37054888082` both confirmed clean
- [x] **Confirmed live (2026-10-03)**: account A's listing set country
      "United States", speed tier "10-50 Mbps", and "usually online" —
      account B's "Find a buddy" page rendered exactly that: "United
      States · 10 – 50 Mbps · Usually online", with "relay-only" and the
      note correctly absent since those were left blank/unchecked. Fields
      really do save and render correctly, confirmed by sight, no DB
      query needed.
- [ ] Those same fields are only sent to the API when "List this
      publicly" is checked (`invite-criteria-rows` toggle) — confirm an
      unchecked private invite doesn't submit stale values from a
      previously-checked session
- [x] **Confirmed live (2026-10-03)**: navigated straight to
      `app.filegarden.net/find-a-buddy.html` (not via a dashboard link) —
      loaded correctly, size dropdown populated, listings rendered. The
      shared `api-utils.js`/`pledge-options.js` scripts load fine given
      Caddy's static root is `apps/web`, same directory.
- [x] **Confirmed live (2026-10-03)**: "Load more" correctly stayed
      hidden throughout, since every size filter tested had only 0 or 1
      listing. `?limit=`/`?offset=`/`has_more`/`total_count` with a real
      50+ listing page still untested — low priority, nothing about the
      mechanism looks fragile and it's not easy to produce for real
      without a lot of throwaway listings.
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

- [x] ~~Pledge-enforcement rejection~~ — stale, contradicted the
      "Pledge-cap rejection, functionally confirmed (2026-10-02)" entry
      above and today's reconciliation close-out, both of which exercise
      this extensively (`pledge-test-1`/`-3`/`-5`, rejected correctly on
      every cycle for over half an hour straight). Removing as a
      duplicate rather than leaving two contradictory lines.
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
- [ ] **Syncthing-style dashboard visibility — Medium and Bigger-lift
      tiers.** Spencer shared a screenshot of Syncthing's Folders/"This
      Device" panels as a template and asked for a cheap tier now, the
      rest later (the cheap tier — STOP_SENDING reason codes surfaced on
      the dashboard, plus version/uptime/local-file-count — shipped
      below). Remaining, roughly in order of how much they'd cost:
  - Medium:
    - ~~Global State rollup~~ — **done** (2026-10-03), see "Needs testing
      — reconciliation against the buddy's actual state" above:
      `backup::reconcile_with_buddy` computes the manifest-vs-buddy
      comparison every 10 minutes per buddy, and it's now surfaced on the
      dashboard buddy card.
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
