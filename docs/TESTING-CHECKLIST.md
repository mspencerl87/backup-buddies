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

- [ ] testb's `.env` pledge is 500GB against a ~47.9GB real disk — fine
      for what's been tested so far, but needs to be realistic (under
      ~37GB free) to actually exercise the pledge-rejection path above
- [ ] `apps/api/src/routes/stripe-webhook.js` — open product decision
      (not a bug): on `invoice.payment_failed`, relay access is paused
      immediately. TODO in that file asks whether a grace period should
      come first.

## Documented, intentional limits (no action needed unless priorities change)

- Backup cycle is a fixed ~30s, not configurable
- No per-file/pattern exclude list for what gets backed up
- Local status dashboard has no login of its own

## Flagged, not yet scoped

- [ ] Restore UX for large transfers: a restore blocks the "Files from
      <buddy>" modal with a single "Restoring…" message and no
      per-file/byte progress until the whole batch finishes — fine for
      a 2MB test, but gives no feedback on a multi-GB/TB restore and no
      way to tell it's still working vs. stuck. Raised 2026-10-02.
      Worth deciding: does the restore need to keep running if the
      modal is closed (it already does — closing the modal doesn't
      cancel the in-flight fetch, it just stops showing the result),
      and does it need actual progress (file N of M, bytes so far) or
      just a way to walk away and check back without losing that state?
