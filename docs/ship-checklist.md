# engram v1 Ship-Readiness Checklist

This is the human-judgment gate for shipping v1.0. It pairs with the automated
`engram spec verify` runner ([#103]) — **both must pass** before a release tag
is cut.

This is a soft gate. The user makes the call. The checklist exists to prevent
shipping on autopilot.

**How to use:** work through each section top-to-bottom. Check each box only
when you have direct, first-hand evidence — not because the code looks right.
Record the date you checked each box in the parenthetical. File a blocking issue
if anything fails; do not ship around it.

---

## 1 · Functional

- [ ] All acceptance criteria in `SPEC.md` verified green by `engram spec verify`
      ([#103]) — paste the run URL here: \_\_\_
- [ ] `cargo test --workspace` passes clean (zero failures, zero ignored-but-broken)
      on the commit you intend to tag
- [ ] CI on `main` has been green for ≥ 7 consecutive days (no red-then-revert
      patches in that window) — link to the Actions run history: \_\_\_

## 2 · Performance

- [ ] Performance budget verification harness ([#109]) has reported green for
      ≥ 30 consecutive nightly runs — link to the dashboard or run log: \_\_\_
- [ ] No budget line regressed more than 10 % vs. the last green baseline without
      an intentional, tracked exception filed in the tracker

## 3 · Security

- [ ] Threat model verification harness ([#110]) reports green on the release commit
- [ ] `cargo audit` reports zero advisories of severity ≥ medium (paste output): \_\_\_
- [ ] `cargo deny check` reports zero denials
- [ ] No secrets, API keys, or PII appear in `git log` for the v1 branch (run
      `git log -p | grep -i 'sk-\|api_key\|password\|token'` and confirm empty)

## 4 · Personal-use validation (dogfooding)

- [ ] Self-dogfooded for ≥ 60 continuous days before the release date (started: \_\_\_)
- [ ] ≥ 3 of the 5 core agents (Scribe, Cartographer, Gardener, Synthesizer,
      Inquirer) have produced output the user found genuinely valuable — record
      one concrete example per agent in a "dogfooding journal" note in the vault
      and link it here: \_\_\_
- [ ] No show-stopping friction has gone unfiled as a bug during the dogfood period

## 5 · Cost validation

- [ ] 30-day actual token cost ≤ configured ceiling for ≥ 90 consecutive days of
      light-to-normal use — paste the cost dashboard summary: \_\_\_
- [ ] No single agent has consumed > 20 % of the total monthly budget without a
      known, accepted reason

## 6 · Recovery validation

- [ ] At least one full "delete SQLite, rebuild from vault + git" recovery exercise
      performed and completed successfully — date: **_, duration: _**
- [ ] The rebuilt index matched the pre-deletion state to the satisfaction of the
      user (spot-checked ≥ 10 queries before and after)

## 7 · Backup validation

- [ ] Vault has been pushed to the designated git remote continuously for ≥ 30 days
      (check `git log --remotes` on the vault repo)
- [ ] At least one "restore from remote" exercise completed successfully — date: \_\_\_
- [ ] Backup Watcher alerts were received and acknowledged on at least one occasion
      where the backup genuinely lapsed

## 8 · Documentation

- [ ] Install guide reviewed end-to-end on a clean machine (or clean user account)
      and works without deviation from the written steps ([#140])
- [ ] First-run wizard tested on a fresh vault and on an existing Obsidian vault;
      both paths complete without errors ([#135])
- [ ] Troubleshooting guide covers every error class encountered during dogfooding
- [ ] Architecture overview is accurate to the shipped code (spot-check ≥ 3 claims
      against the actual implementation)

## 9 · Real-vault test

- [ ] The 9K-note Obsidian vault smoke test ([#108]) has been run to completion
- [ ] Curator compression ratio achieved ≥ 5:1 on the test vault
- [ ] User has reviewed a random sample of ≥ 50 discard decisions and found the
      judgment acceptable (not necessarily agreed with every call — "acceptable"
      means no systematic bias, hallucination, or safety failure)
- [ ] No note the user considers irreplaceable was discarded without proposal review

## 10 · Ship-readiness retrospective

- [ ] `docs/v1-retrospective.md` written and committed, covering:
  - What was hardest (technically and personally)
  - What changed scope from the original design and why
  - What the user would do differently
  - What to carry forward into v2 (both momentum and caution)
- [ ] Retrospective has been read at least 24 hours after writing (fresh eyes catch
      things written in the heat of completion)

---

## Final gate

Only proceed if all boxes above are checked:

- [ ] **I, the user, have read every item above and believe engram v1 is ready to
      tag as a stable personal release.**
- [ ] Release tag: `v1.0.0` — date: \_\_\_

---

## References

- [`SPEC.md`](../SPEC.md) — machine-readable v1 acceptance criteria
- [`docs/design/07-roadmap.md`](design/07-roadmap.md) — v1 scope and acceptance summary
- [#103] — `engram spec verify` automated runner
- [#108] — 9K-vault smoke test
- [#109] — performance budget verification harness
- [#110] — threat model verification harness
- [#135] — first-run wizard
- [#140] — user-facing documentation

[#103]: https://github.com/torsday/engram/issues/103
[#108]: https://github.com/torsday/engram/issues/108
[#109]: https://github.com/torsday/engram/issues/109
[#110]: https://github.com/torsday/engram/issues/110
[#135]: https://github.com/torsday/engram/issues/135
[#140]: https://github.com/torsday/engram/issues/140
