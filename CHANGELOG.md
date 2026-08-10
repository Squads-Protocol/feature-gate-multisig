# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-08-10

Hardens the path that decides what a proposal is and what a signer is told it
does. Supersedes 0.3.0, whose transaction classifier labels any decodable config
transaction a rekey and any all-System vault message an activation.

### Changed

- `--yes` no longer authorizes an action the tool cannot vouch for. It aborts on
  an unrecognized proposal, and refuses a config change outright rather than
  resolving the confirmation on the operator's behalf. Recognized activate,
  revoke, and rekey proposals are unaffected.
- `verify` exits non-zero when a check could not be completed. It still reports
  every problem in one run; only the exit code changed.
- An ambiguous `--network` name is an error naming the candidates, instead of
  resolving to whichever endpoint was configured first.
- `--threshold` parses at the on-chain width, so out-of-range values are
  rejected rather than truncated (65537 previously became 1, 65536 became 0).
- An explicit `--threshold` is honoured when the saved configuration is reused,
  instead of being silently discarded.
- Canonical rekeys are labelled as permanently disabling voting, and approving
  or executing one prints the resulting threshold and the number of members able
  to vote afterwards before asking.

### Fixed

- Squads accounts are authenticated, not just decoded: transaction, proposal,
  and multisig reads now require Squads ownership and a record naming the
  multisig and index being read. A multisig is bound to its address through the
  `create_key` PDA derivation.
- A failed multisig read no longer defaults the member list to empty. That
  baseline made a config change which only weakens the threshold identical to a
  canonical rekey, so it was certified as one.
- Proposal classification fails closed. A transaction that cannot be read or
  authenticated is refused rather than warned about.
- The parent multisig flow describes a child by what it is on-chain, rather than
  by the kind the caller passed.
- `show` sweeps the endpoint being inspected, so feature state and the rekey
  warning describe the cluster on screen rather than the saved network list.
- `show --index` prints instruction data in full. Truncation cut the owner
  pubkey out of a System `assign`, which is the field distinguishing an
  activation from a hijack.
- Proposal creation asks before sending, matching every other send path.
- Partial multi-network deployments name the networks that succeeded and warn
  about the ones that did not, instead of reporting completion without naming a
  cluster.
- Executing a proposal that loads accounts from address lookup tables reports
  why this tool cannot, instead of failing on-chain.
- Malformed ProgramData is rejected rather than read as an immutable program.
- A confirmation timeout says the transaction may still have landed, and
  creating a proposal warns when the newest one already matches it.
- Saving a voting key no longer drops the other configured networks when
  `--network` was passed.

## [0.3.0] - 2026-07-13

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
