# Minimum Rust version, 2026-10-08

The advertised Rust 1.85 minimum did not build the locked dependencies.
The declaration, current installation instructions and missing-Cargo message
now require Rust 1.88. A separate CI job checks all workspace targets and
features with both Cargo and rustc pinned to 1.88.0. Existing stable Clippy
rules and test gates remain in force.

- [Rust 1.85 failure](rust-1.85-check.txt) reports 33 errors in the locked
  `serde-saphyr` dependency. This probe used detached `709d965` with its
  original Rust 1.85 declaration, Cargo 1.85.0 and rustc 1.85.0, without
  `--ignore-rust-version`.
- [Rust 1.88 checkpoint](rust-1.88-check.txt), [actual source installation](source-install.txt)
  and [first-launch tour](source-tour.txt) cover `709d965` with only the minimum
  declaration corrected. The install built all seven programs into a new
  prefix. The real Senate choice, six tour steps, measured Arena, Selection,
  replay and restored terminal passed under Node 22. [Checkpoint proof](proof.json)
  keeps this scope separate from later source changes.
- [Current-source compiler check](current-rust-1.88-check.txt) includes the
  explicit Cargo and rustc versions. [Source hashes](current-source-proof.json)
  identify the later syntax changes and updated guardian mutation anchor.
  The all-target/all-feature frozen check passed on Rust 1.88.
- Correcting the minimum activates stable Clippy suggestions for let chains,
  `is_multiple_of` and `as_chunks`. These edits preserve the prior short-circuit
  and chunk behavior. [All-feature](clippy-all.txt) and [default-feature](clippy-default.txt)
  local Clippy checks pass with `-D warnings`.
  [Command and toolchain proof](checkpoint-command-proof.json) records the
  working-tree scope; no hosted CI result is inferred. The [workspace run](workspace-tests.txt)
  passes 715 tests including doc tests, with no failures or ignored tests.
- [All 45 affected mutations](affected-mutations.txt) passed their mandatory
  unmodified baseline and were killed, with zero survivors, stale entries or
  timeouts. [Source hashes and restoration proof](affected-mutations-proof.json)
  identify the tested files and confirm byte-for-byte restoration. The complete
  configured 428-entry matrix passed at the earlier immutable checkpoint; this
  later run covers every mutation in the seven changed source modules.

The compiler check pins both executable paths because Homebrew Cargo/rustc
can otherwise bypass a rustup toolchain selector:

```sh
RUSTC="$(rustup which --toolchain 1.88.0 rustc)" \
  "$(rustup which --toolchain 1.88.0 cargo)" check --locked --workspace --all-targets --all-features
```

The earlier installation checkpoint is not proof of the final package or UI.
Current native package acceptance is recorded with the
[cost-disclosure evidence](../2026-10-08-cost-disclosure/README.md).
