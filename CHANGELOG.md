# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - Unreleased

### Added

- `verify` command: checks, across every configured network, that the Squads
  program is the authentic immutable v4 (verified bytecode hash), reports the
  feature gate account state (fresh/pending/activated) and rent exemption,
  lists the multisig owners, threshold, autonomy, and time lock, and flags
  cross-network config drift. Cluster identity is detected from the genesis
  hash rather than the RPC URL.
- Pre-flight checks before feature gate actions: warn and confirm when the
  action does not match the on-chain feature state.
- Non-interactive proposal subcommands: `propose`, `approve`, `reject`,
  `execute`, with `--multisig`, `--kind`, `--index`, `--voting-key`,
  `--keypair`, `--network`, and `--yes` flags.
- Interactive proposal picker: live proposals are listed with what each one
  does (classified from the on-chain transaction shape), status, and vote
  counts; proposals the chosen action can no longer apply to are filtered out.
- Saved voting-key default; session memory for the multisig address; Esc
  returns to the menu instead of exiting.
- Wrong-address errors explain themselves, and pasting a feature gate account
  looks up and names its multisig from transaction history.
- Source-based release process: versions are git tags, installed via
  `cargo install --locked --git ... --tag <version>` or clone-and-build.

### Changed

- Config moved from the current working directory to the per-user OS config
  directory (`~/.config/feature-gate-multisig-tool/config.json` on Linux).
- Solana dependency stack migrated to the Agave 3.x line; the dependency tree
  shrank from 753 to 463 packages. Direct dependencies are pinned exactly.
- `show` validates account ownership before rendering, EOA voting paths
  enforce membership and permissions up front, and `voting_key` must match
  the fee payer in EOA mode.

## [0.2.0]

Initial public iteration: multisig provisioning across networks, interactive
proposal flows (activate/revoke/rekey), parent-multisig voting, and the
surfpool-backed E2E suite.
