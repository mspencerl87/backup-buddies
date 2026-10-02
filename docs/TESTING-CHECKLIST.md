# Testing checklist

Running list of what's been verified against a live testa/testb pairing,
and what's still open. Update this in place as items get checked off
instead of re-deriving the list from scratch each session.

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

## Needs testing (new — not yet exercised on testa/testb)

Added by the exact-size marketplace matching fix (not yet committed as of
this writing — see "Marketplace size-matching" below for what changed):

- [ ] "Find a buddy" only ever shows one pledge size at a time (the
      `marketplace-size` dropdown) — confirm switching it actually
      re-queries and swaps the listed buddies, including the
      now-you-see-it/now-you-don't case of a size with zero listings
      (the empty-state message)
- [ ] Redeeming a listing sends the selected size back to the API
      (`pledged_bytes` in the POST body) and the resulting pairing's
      `pledged_bytes` matches exactly — confirm end-to-end on
      testa/testb, not just by reading the code
- [ ] `GET /invites/public` without a `pledged_bytes` query param returns
      400 — confirm the dashboard never calls it that way and that a
      direct/manual API call gets the expected rejection
- [ ] The redeem-time mismatch guard (`expectedPledgedBytes` check in
      `redeemInvite`) actually fires on a 409 if forced — this needs a
      deliberate test (e.g. two browser tabs, or a raw `curl` with a
      `pledged_bytes` that doesn't match the listing id) since the normal
      UI flow can't produce a mismatch on its own
- [ ] The `pledged-gb` (create) and `marketplace-size` (browse) dropdowns
      still show the same set of sizes after any future
      `PLEDGE_STEP_BYTES`/`PLEDGE_MAX_BYTES` change — they're generated
      from one shared `PLEDGE_GB_OPTIONS` list in `dashboard.html` now,
      but that list is hand-maintained and not pulled from the API, so a
      future API-side step/max change needs the same edit made here too

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

Not yet committed/pushed as of this writing — see "Needs testing" above
for what to confirm on testa/testb before and after it ships.

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
