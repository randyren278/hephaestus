# Native release rehearsal, 2026-10-08

[GitHub run 37860441104](https://github.com/randyren278/hephaestus/actions/runs/37860441104)
completed successfully at `739cb9cb71acd0a83f980bdc21652ea6006399c7` through
manual dispatch. All five build/check jobs passed; **Sign and attest** and
**Publish GitHub Release** were skipped. No tag or release was created.

[Proof](proof.json) records the terminal job results, artifact identities,
archive checksums, native Mach-O architecture and per-binary SHA-256 values.
Codex downloaded both accepted archives through GitHub CLI, checked their
checksums and metadata, and confirmed the owner's private HTML guide is absent.
GitHub artifact digests identify the uploaded artifact containers; archive
SHA-256 values identify the tarballs inside them. The archives remain unsigned
rehearsal artifacts and are not an authenticated public release.

## Installed acceptance

Both native jobs ran the unchanged installed-package acceptance script:

- [Apple Silicon output](acceptance-arm64.log) and
  [archive checksum](hephaestus-v0.1.0-macos-arm64.tar.gz.sha256).
- [Intel output](acceptance-x86_64.log) and
  [archive checksum](hephaestus-v0.1.0-macos-x86_64.tar.gz.sha256).

Each job passed isolated-home installation, relocation, the actual six-step
terminal tour, fixture initialization, offline Arena/replay, bundled terminal
and browser consoles, frozen hosted-pilot preparation and both reference
Gauntlets. Host Node/npm were hidden; Git and harness Python were present.
Hosted-provider fixtures are local scripts, not actual model calls or proof
of fresh-user provider authentication.

Both target-specific Rust dependency inventory jobs also passed.
[Reproducibility output](reproducibility.log) confirms identical ARM64 archive
hashes for two builds on one runner with the same checkout/toolchain. These
hashes also match the separate accepted ARM64 job's archive. This is not an
independent third-party reproduction on a differently provisioned builder.
The reproducibility log export strips trailing whitespace; the proof records
both the original job-line and exported hashes. The original downloaded
workflow log remains local.

## Review and verification

An [actual read-only Claude Code review](claude-review.txt) checked these
records against the workflow and guides. Its missing architecture-record
finding is addressed by the recorded Mach-O header checks and native
`file` classifications for every Rust binary and bundled Node. Its
reproducibility wording correction is reflected above. Acceptance file names
are explicit in the proof, and the current installation/readiness descriptions
retain the fresh-user boundaries. Claude did not query GitHub or run binaries.
Codex separately verified terminal job results, downloaded archive bytes and
checksums, and ran the [documentation checks](docs-check.txt).

## Remaining delivery requirements

This closes native Intel package acceptance and the manual release pipeline
rehearsal. Actual Sigstore signing, provenance attestation, release publication
and first launch from a quarantined browser download remain unverified.
Apple Developer ID signing/notarization is not implemented. CLI artifact
download and hosted runner acceptance do not substitute for that fresh-user
launch. Attributable model USD, independent customer acceptance and an
available commercial offer also remain open.
