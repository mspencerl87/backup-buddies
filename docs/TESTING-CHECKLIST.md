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
- [ ] Apply to testa/testb: update both `docker-compose.yml`s (add
      `network_mode: host`, drop the now-ignored `ports:` section),
      confirm `DASHBOARD_PORT`/`SYNC_PORT` are still distinct between the
      two (already true: 8080/8081 and 11235/11236), pull the new image,
      restart both, and confirm the dashboard is still reachable at its
      usual address/port after losing Docker's port mapping
- [ ] Real test: two clients on an actual shared LAN (e.g. two machines
      on the same home wifi, not same-host containers) — confirm the
      `"found a buddy on the local network"` log line appears on both
      sides and the dashboard's buddy card shows direct, not relay
- [ ] Confirm two buddies on *different* networks still behave exactly
      as before this change (mDNS finds nothing, falls through to
      relay/SYNC_PORT direct-over-internet) — must be a strict addition,
      never a regression
- [ ] Confirm container restart doesn't leave stale mDNS records
      (re-advertise cleanly on startup, old record expires on the other
      side within a reasonable time — `DiscoveryEvent::Expired`)

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
