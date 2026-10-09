# Release publication safeguards and deterministic floor fixtures

The publisher now names the repository explicitly, discovers private drafts
through paginated release listings, rejects duplicate tag matches, and pins
readbacks to the release ID. It binds the remote tag to the requested source
commit and checks the draft's source marker, rather than relying on GitHub's
non-authoritative target_commitish field. It verifies all 36 asset names,
uploaded states, sizes and SHA-256 digests before publication. Public retries
verify source, channel and bytes without changing public assets. Successful
non-API GitHub CLI output is not parsed as JSON. The first channel is a
prerelease, without marking it Latest.

The signing job also verifies every signature and source-bound attestation
before passing assets to publication. Manual workflow dispatch remains
nonpublishing. No release tag, hosted signature or public release was created.

## Evidence and limits

- 15 local stateful GitHub CLI contracts passed, including interrupted uploads,
  recovery, corrupt/pending assets, moved tags, lightweight tags, duplicate
  drafts, unrelated drafts, public retry/channel refusal, symlinks, API failure,
  and successful create/edit URL output. These are offline API simulations,
  not hosted signing or publication acceptance.
- 11 package contracts and nine source-install contracts passed.
- The installed GitHub CLI reproduced missing repository routing from an empty
  directory. With explicit repository identity, it attempted only the closed
  loopback endpoint. No mutation or external request was made in that check.
- Actual Claude Code performed read-only reviews. The first final review caught
  non-JSON CLI URL output; the implementation and fixture were corrected. The
  followup found no remaining concrete blocker. Static review is not testing.
- CI run [37861565713](https://github.com/randyren278/hephaestus/actions/runs/37861565713)
  failed at eb15dbc because ambient scheduling made the supposedly slower
  candidate faster in the small-latency drift fixture. Three floor/replay
  fixtures now intercept real worker output and supply 10ms parent / 25ms
  candidate readings before the canonical writer signs them. Only the clock
  input is fixed; workers, signing, scoring, floor decisions and replay remain
  exercised. The setter exists only under cfg(test). Production thresholds
  and the mutation manifest are unchanged.
- All three restored-source floor/replay tests passed. All four existing floor
  and old-rule replay mutations were killed with their own manifest commands,
  with no survivors, stale entries or timeouts. The focused mutation run reused
  the separately proven unmutated test baselines. An earlier broad filter
  omitted the old-rule drift test; that filter was discarded and all four
  checks were rerun with their declared exact commands.

Workflow concurrency serializes its own runs. Operators must not manually edit
or publish the draft during uploads; the API has no atomic check-and-publish
transaction. Fresh-user quarantine/provider acceptance, actual hosted signing
and independent commercial value remain open. The private owner HTML guide is
outside this repository and release assets.
