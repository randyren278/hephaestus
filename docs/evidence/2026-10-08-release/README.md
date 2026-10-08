# Release gate review, 2026-10-08

The release workflow now requires installed acceptance on each native macOS
architecture before uploading or signing its archive. ARM64 uses macos-15;
Intel uses macos-15-intel, with an explicit host-architecture assertion.
Ordinary pull-request and main CI also include ARM64 package acceptance.
These are workflow changes, not evidence of successful hosted runs.

Actual local checks:

- Actionlint 1.7.12 parsed both changed workflows with exit 0. Its official
  ARM64 binary was SHA-256 checked against the GitHub release asset digest.
- [Workflow shell proof](workflow-shell-proof.json) executes the actual
  version and acceptance run blocks in scratch directories. A matching tag
  and a branch rehearsal pass; a mismatched tag rejects without writing a
  version output. Acceptance exit 23 propagates through tee and preserves
  diagnostics; exit 0 passes. The acceptance script is a fixture here;
  [real installed acceptance](../2026-10-08-install/README.md) is separate.
- [Workflow gates](workflow-gates.json) record signing prerequisites and
  execute the actual publication expression for tag push, manual tag,
  manual branch and ordinary branch cases. The same guard applies to signing
  and attestation; manual dispatch does neither and never publishes.
- [Actual inventories](sbom-proof.json) were generated with cargo-cyclonedx
  0.5.9 and collected using the workflow's Python run block. All seven
  shipped Rust binaries have CycloneDX 1.5 inventories for both macOS
  targets. Target-suffixed filenames avoid collisions when artifacts merge.
  This is target-specific dependency metadata, not native Intel execution.
- [Cosign bundle proof](cosign-bundle-proof.json) uses Cosign 3.1.3 to verify
  its own official upstream binary and bundle. The expected upstream identity
  passes; the Hephaestus release identity rejects. This demonstrates bundle
  CLI compatibility and exact identity enforcement, not Hephaestus signing.
- [CLI help](cosign-sign-help.txt) and [inventory help](cyclonedx-help.txt)
  were captured from the exact installed tool versions. Version metadata and
  downloads came from the official Sigstore GitHub release and crates index.
- Documentation and whitespace gates passed after the edits.

Inventories are generated in separate jobs with read-only repository
permissions, no persisted checkout credential, no accepted archives and no
signing permissions. Signing waits for both native builds, both inventory
jobs and the existing reproducibility job. Inventories, archives and
checksums receive Sigstore bundles; archives and inventories also receive
GitHub provenance attestations. Verification documentation constrains the
expected repository, workflow and version tag. See
[release verification](../../RELEASES.md).

The [initial Claude review](claude-initial-review.txt) identified the broad
signer identity, missing tag/version guard, unpinned inventory generator,
shared archive/SBOM job and legacy Cosign flags. These changes address those
findings. The [final review](claude-final-review.txt) found that a tag
rehearsal could create an official-looking signing identity. Signing and
attestation now have the same tag-push-only guard as publication. Its narrow
source-install and deterministic-tour evidence assessment was positive.
The [follow-up review](claude-followup-review.txt) confirms that the event
guard resolves the issue and reports no remaining introduced blockers.
Its optional credential-retention note was also addressed in reproducibility.

Primary references checked on this date:

- [GitHub hosted runner architectures](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
- [Cosign blob signing and bundles](https://docs.sigstore.dev/cosign/signing/signing_with_blobs/).
- [cargo-cyclonedx 0.5.9 options](https://github.com/CycloneDX/cyclonedx-rust-cargo/blob/cargo-cyclonedx-0.5.9/cargo-cyclonedx/README.md).
- [Pinned attestation action inputs](https://github.com/actions/attest-build-provenance/blob/4d101475d8b20a2381f78447822ac1eab6504dd8/action.yml).

Hosted CI, native Intel acceptance, Hephaestus keyless signing, actual release
download and quarantine behavior remain unverified. The public rulesets API
returned no rulesets; this does not establish every classic protection or
private repository setting. Tag governance remains an owner-controlled
release requirement. No release, push or repository-setting mutation occurred.
