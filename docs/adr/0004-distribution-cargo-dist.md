# ADR-0004: Distribution via dist (cargo-dist)

- Status: Accepted / Date: 2026-07-02

## Context
v0.1 launch requires "day-one distribution complete": prebuilt binaries + Homebrew +
cargo-binstall the moment we tag, matching the common practice of comparable Rust CLIs.
We need a tool that generates a release pipeline (build matrix, archives, checksums,
installers, GitHub Release, Homebrew formula push) from a single config.

Tooling status had to be verified rather than assumed (rules/behavior.md, no guessing):
- `axodotdev/cargo-dist` is the original tool; the binary is `dist`.
- `astral-sh/cargo-dist` was an unofficial fork of 0.28.0; it was **archived 2025-12-19**
  with a note that upstream is active again and has absorbed the fork's changes.
- `axodotdev/cargo-dist` latest is **0.32.0 (2026-05-22)** and actively maintained.

Sources:
- https://github.com/astral-sh/cargo-dist (archived, "refer to axodotdev/cargo-dist instead")
- https://github.com/axodotdev/cargo-dist/releases (0.32.0, active)
- https://axodotdev.github.io/cargo-dist/book/installers/homebrew.html (tap + HOMEBREW_TAP_TOKEN)

## Decision
- Use **`dist` (axodotdev/cargo-dist) 0.32.0**, config in `dist-workspace.toml`
  (`[workspace] members = ["cargo:."]` + `[dist]`), plus `[profile.dist]` in the root Cargo.toml.
- Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`. Installers: `shell` + `homebrew`. Unix archive: `.tar.xz` with `.sha256`.
- Homebrew tap: `rioX432/homebrew-tap` (public), formula pushed by the `publish-homebrew-formula`
  job using a `HOMEBREW_TAP_TOKEN` repo secret (PAT with `repo` scope). GITHUB_TOKEN cannot push
  to a second repo, so the token secret is mandatory; the release is otherwise green but the tap
  won't update without it.
- cargo-binstall support via explicit `[package.metadata.binstall]` in `crates/agent-witness/Cargo.toml`,
  matching dist's archive naming/layout, verified with `dist build` (archive
  `agent-witness-<target>.tar.xz` holds a top-level `agent-witness-<target>/` dir with the binary).
- Release workflow `.github/workflows/release.yml` is dist-generated (do not hand-edit); triggered on
  version tags. `pr-run-mode = "plan"` so PRs validate without uploading. CI gate (`ci.yml`) is unchanged.

## Consequences
Upside: one config drives binaries/brew/binstall/checksums; regenerate on `dist` upgrades via
`dist generate`. Downside: release.yml is generated (must re-run `dist generate` after config/version
changes, never hand-edit) and the maintainer must add the `HOMEBREW_TAP_TOKEN` secret before the first
real tag or the tap step fails.
