# Feature Gate Multisig Tool

CLI tool for creating and managing Solana feature gate multisigs using the Squads protocol. Enables collective governance over Solana feature activations and revocations.

## Installation

The tool is distributed as source: you build the exact code you reviewed, and
`--locked` builds use the committed lockfile, which pins every dependency by
version and checksum.

Requires a Rust toolchain ([rustup](https://rustup.rs)), and `libudev-dev` +
`pkg-config` on Linux (for hardware-wallet support).

To check hardware-wallet support in your build: any command against a
`usb://ledger` fee payer with no device plugged in must fail with "no device
found", not "hidapi crate compilation disabled" (Ledger signing cannot work in
that build).

Install a released version:

```bash
cargo install --locked --git https://github.com/nbelenkov/feature-gate-multisig --tag <version>
```

Or clone and build:

```bash
git clone https://github.com/nbelenkov/feature-gate-multisig.git
cd feature-gate-multisig
git checkout <version>   # pick a release tag; omit for the development branch
cargo build --release --locked
```

Binary: `./target/release/feature-gate-multisig-tool`

Releases are announced as git tags; see [CHANGELOG.md](CHANGELOG.md) for what
each version contains.

## Quick Start

```bash
# Interactive mode (recommended)
feature-gate-multisig-tool

# Direct commands
feature-gate-multisig-tool create --keypair ~/.config/solana/id.json
feature-gate-multisig-tool show <MULTISIG_ADDRESS>
feature-gate-multisig-tool config
```

## Commands

| Command | Description |
|---------|-------------|
| `create` | Create a new feature gate multisig with an activation proposal |
| `show <address>` | Display multisig details, members, and proposal status |
| `verify <address>` | Check program authenticity, feature state and owners on every configured network |
| `propose` / `approve` / `reject` / `execute` | Act on a proposal non-interactively |
| `check-signer` | Confirm a keypair (including `usb://ledger`) can act on a multisig; signs nothing |
| `config` | Show saved configuration |
| `interactive` | Launch interactive menu (default) |

### Before an activation: check every signer

```bash
feature-gate-multisig-tool check-signer --keypair usb://ledger --multisig "$MULTISIG"
```

Resolves the key (device present and unlocked; signs nothing) and reports
whether it is a voting member of the multisig. Non-zero exit means that signer
cannot act: wrong device, derivation path, or multisig, or a build without
hardware-wallet support. Run it days ahead, not at the first signature.

### Scripting

`verify` exits non-zero when a check fails **or** cannot be completed, so it can
gate a runbook:

```bash
feature-gate-multisig-tool verify "$MULTISIG" && \
  feature-gate-multisig-tool approve --multisig "$MULTISIG" --kind activate --index 1 --yes
```

It still reports every problem in one run rather than stopping at the first.
A non-zero exit means the multisig has not been shown to be correct - not
necessarily that it is malicious; an unreachable network fails the same way.

`verify` fails when the multisig is not autonomous, has been rekeyed
(permanently frozen), or its voting members differ from the expected set:
`KNOWN_SIGNERS` when vendored into the build, otherwise the `members` list in
your config. With neither it says so, and fails on mainnet. Keep config
`members` equal to the agreed signer set and `verify` catches a swapped, added,
or removed owner.

`propose`, `approve`, `reject`, and `execute` print the action, multisig,
feature gate, and network before signing; `--yes` does not suppress this. They
also refuse up front when the proposal's status or staleness means the action
cannot succeed.

`--yes` resolves each confirmation to its default answer. It does not force an
action through: a proposal this tool cannot classify, and any config change
(membership or threshold), abort under `--yes` and need an interactive decision.

## Interactive Mode

> **See [docs/WORKFLOWS.md](docs/WORKFLOWS.md) for detailed step-by-step examples.**

The interactive menu provides:
- **Create new feature gate multisig** - Guided setup with member collection
- **Show multisig details** - Inspect any multisig address
- **Proposal Actions** - Create/Approve/Reject/Execute proposals
- **Show configuration** - View saved settings

### Proposal Actions

Supports three transaction types:
- **Activate Feature Gate** - Enable a Solana feature gate
- **Revoke Feature Gate** - Cancel a pending activation
- **Rekey Multisig** - Brick the multisig (rekey use only)

Each supports: Create, Approve, Reject, Execute

## Proposal Structure

When a multisig is created, one proposal is automatically generated:

| Index | Type | Purpose |
|-------|------|---------|
| 1 | Vault Transaction | Feature Activation |

**Note**: Activating or revoking a feature gate does not change the multisig threshold.

Revocation proposals are **not** pre-created. If you need to revoke a feature, create a new revocation proposal using "Proposal Actions" → "Revoke Feature Gate" → "Create". See [Emergency Revocation workflow](docs/WORKFLOWS.md#4-emergency-revocation) for details.

## Parent → Child Multisig Voting

For programmatic voting from a parent multisig:

> **Important**: Add the parent's **vault PDA** (not multisig address) as a member of the child multisig with Vote/Execute permissions.

The **fee payer keypair** must be a member of the parent multisig with full permissions. The fee payer's signature is used to create/approve/execute proposals on the parent multisig.

The parent multisig address cannot sign during CPI - only the vault PDA can be signed for using PDA seeds.

```
Parent vault PDA = get_vault_pda(parent_multisig, 0)
```

The CLI will display the required PDA if misconfigured. See [Parent Multisig Voting workflow](docs/WORKFLOWS.md#3-activating-a-feature-parent-multisig-voting) for detailed steps.

## Configuration

Stored in your OS config directory (for example `~/.config/feature-gate-multisig-tool/config.json` on Linux, `~/Library/Application Support/feature-gate-multisig-tool/config.json` on macOS). Run `feature-gate-multisig-tool config` to print the exact path:

```json
{
  "threshold": 2,
  "members": ["<pubkey1>", "<pubkey2>"],
  "networks": ["https://api.devnet.solana.com"],
  "fee_payer_path": "usb://ledger"
}
```

| Field | Description |
|-------|-------------|
| `threshold` | Required signatures |
| `members` | Saved member public keys |
| `networks` | RPC endpoints for deployment |
| `fee_payer_path` | Keypair path (file or `usb://ledger`) |

## Network Support

- Mainnet: `https://api.mainnet-beta.solana.com`
- Devnet: `https://api.devnet.solana.com`
- Testnet: `https://api.testnet.solana.com`
- Custom RPC endpoints supported

## Testing

The e2e harness auto-answers prompts via `E2E_TEST_MODE`, which only exists
behind the `e2e-harness` cargo feature and is **compiled out of release
builds**. `make test-surfpool` enables it; such builds print a warning banner
on every run.

```bash
# Unit tests
cargo test

# E2E tests (requires surfpool)
make test-surfpool
```

### E2E Setup

```bash
# Install surfpool
curl -sL https://run.surfpool.run/ | bash
# Or: brew install txtx/taps/surfpool

# Run tests
make test-surfpool
```

## License

MIT - see [LICENSE](LICENSE)
