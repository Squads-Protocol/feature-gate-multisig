//! Verification helpers for vetting a feature gate before acting on it.
//!
//! Two independent dimensions:
//! - Feature gate validity: the on-chain state of the feature account (the
//!   multisig's vault PDA) and whether it matches the action being taken.
//! - Squads program authenticity: that the program at [`SQUADS_MULTISIG_PROGRAM_ID`]
//!   on the target cluster is the genuine, immutable Squads v4 program.

use crate::commands::TransactionKind;
use crate::feature_gate_program::{FEATURE_ACCOUNT_SIZE, FEATURE_GATE_PROGRAM_ID};
use crate::squads::{get_vault_pda, Multisig, SQUADS_MULTISIG_PROGRAM_ID};
use eyre::{eyre, Result};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use std::str::FromStr;

/// Upgradeable BPF loader that owns deployed programs and their ProgramData accounts.
pub const BPF_LOADER_UPGRADEABLE_ID: Pubkey =
    Pubkey::from_str_const("BPFLoaderUpgradeab1e11111111111111111111111");

/// Canonical on-chain hash of the official, immutable Squads v4 program.
///
/// This is `sha256` over the deployed ELF with trailing zero bytes trimmed,
/// the same value `solana-verify get-program-hash` and OtterSec's `on_chain_hash`
/// report (commit `6d5235d`). The program is frozen (no upgrade authority), so
/// the deployed bytecode, and therefore this hash, never changes.
pub const SQUADS_V4_PROGRAM_HASH: &str =
    "d48660833989ecea3145ff726164fe640bd90696f03ce00dfd0cda258cbf2fac";

/// Fixed size of the upgradeable-loader ProgramData metadata header that
/// precedes the ELF: 4-byte enum tag + 8-byte slot + 1-byte option + 32-byte authority.
const PROGRAMDATA_HEADER_LEN: usize = 45;
/// Borsh discriminant for `UpgradeableLoaderState::ProgramData`.
const PROGRAMDATA_ENUM_TAG: [u8; 4] = [3, 0, 0, 0];

/// Genesis hash of Solana mainnet-beta.
pub const MAINNET_GENESIS_HASH: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";

/// Detect whether `rpc` points at Solana mainnet by comparing the cluster's
/// genesis hash against the known mainnet genesis hash. Unlike URL matching,
/// this identifies the cluster from the chain itself, so custom RPC endpoints
/// are classified correctly. Mainnet forks/simulators (e.g. surfpool) report
/// mainnet's genesis hash and classify as mainnet, which is intended: they
/// carry mainnet state, so the strict mainnet checks apply.
pub fn is_mainnet_cluster(rpc: &RpcClient) -> Result<bool> {
    let genesis = rpc
        .get_genesis_hash()
        .map_err(|e| eyre!("failed to fetch genesis hash: {e}"))?;
    Ok(genesis.to_string() == MAINNET_GENESIS_HASH)
}

/// Decide how strictly to check `rpc`, in a way the endpoint cannot relax.
///
/// Two independent signals: the chain's genesis hash, and whether the operator's
/// URL names mainnet. Either one asserting mainnet is enough, because the only
/// dangerous direction is *downgrading* - answering "not mainnet" is what skips
/// the immutability and bytecode-hash assertions, so an endpoint that could do
/// that would be choosing how strictly it is checked.
///
/// A mainnet fork on a custom URL (surfpool) therefore stays strict, which is
/// intended: it carries mainnet state. A URL naming mainnet while the chain says
/// otherwise is the substitution this refuses outright.
///
/// Returns the cluster plus whether the chain itself could be asked, so callers
/// can record an unverified cluster as incomplete.
pub fn resolve_cluster(rpc: &RpcClient, rpc_url: &str) -> Result<(bool, bool)> {
    let by_genesis = is_mainnet_cluster(rpc).ok();
    strictness_for(by_genesis, crate::utils::is_mainnet(rpc_url), rpc_url)
}

/// The decision itself, separated from the RPC call so the asymmetry is testable.
/// `by_genesis` is None when the chain could not be asked.
fn strictness_for(by_genesis: Option<bool>, by_url: bool, rpc_url: &str) -> Result<(bool, bool)> {
    match by_genesis {
        Some(false) if by_url => Err(eyre!(
            "Cluster mismatch: {rpc_url} names mainnet but its genesis hash is not mainnet's. \
             Refusing to continue - relaxing the audit here is exactly what a substituted \
             endpoint would want."
        )),
        Some(by_genesis) => Ok((by_genesis || by_url, true)),
        // Could not ask the chain. Fall back to the URL, which is at least not
        // the endpoint's choice, and report the cluster as unverified.
        None => Ok((by_url, false)),
    }
}

/// On-chain state of a feature gate account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureGateStatus {
    /// Not provisioned: owned by the System program (or does not exist yet).
    Fresh,
    /// Owned by the Feature Gate program, queued but not yet activated.
    Pending,
    /// Activated at the given slot.
    Activated { slot: u64 },
    /// Anything else: unexpected owner or data length.
    Unexpected { owner: Pubkey, data_len: usize },
}

/// Classify a feature account from its owner and raw data.
pub fn classify_feature_account(owner: &Pubkey, data: &[u8]) -> FeatureGateStatus {
    if *owner == FEATURE_GATE_PROGRAM_ID {
        // A Feature account is `Option<u64>`: 1-byte tag + 8-byte slot.
        if data.len() == FEATURE_ACCOUNT_SIZE && data[0] == 0 {
            return FeatureGateStatus::Pending;
        }
        if data.len() == FEATURE_ACCOUNT_SIZE && data[0] == 1 {
            let slot = u64::from_le_bytes(data[1..9].try_into().unwrap_or([0u8; 8]));
            return FeatureGateStatus::Activated { slot };
        }
        return FeatureGateStatus::Unexpected {
            owner: *owner,
            data_len: data.len(),
        };
    }
    if *owner == solana_system_interface::program::ID {
        return FeatureGateStatus::Fresh;
    }
    FeatureGateStatus::Unexpected {
        owner: *owner,
        data_len: data.len(),
    }
}

/// Result of inspecting a multisig's feature gate account (vault 0).
#[derive(Debug, Clone)]
pub struct FeatureVerification {
    pub feature_id: Pubkey,
    pub status: FeatureGateStatus,
    pub lamports: u64,
    pub rent_exempt: bool,
}

/// Fetch and classify the feature gate account (vault 0) of `multisig`.
pub fn verify_feature_gate(rpc: &RpcClient, multisig: &Pubkey) -> Result<FeatureVerification> {
    let (feature_id, _) = get_vault_pda(multisig, 0);
    let rent_min = rpc.get_minimum_balance_for_rent_exemption(FEATURE_ACCOUNT_SIZE)?;

    let (status, lamports) = match rpc.get_account(&feature_id) {
        Ok(acc) => (
            classify_feature_account(&acc.owner, &acc.data),
            acc.lamports,
        ),
        Err(e) if account_not_found(&e.to_string()) => (FeatureGateStatus::Fresh, 0),
        Err(e) => {
            return Err(eyre!(
                "failed to fetch feature account {}: {}",
                feature_id,
                e
            ))
        }
    };

    Ok(FeatureVerification {
        feature_id,
        status,
        lamports,
        rent_exempt: lamports >= rent_min,
    })
}

/// Advisory warnings for performing `kind` against the current feature state.
/// Never blocks; callers surface these and confirm to proceed.
pub fn feature_action_warnings(v: &FeatureVerification, kind: TransactionKind) -> Vec<String> {
    let mut warnings = Vec::new();
    match kind {
        TransactionKind::Activate => {
            if let FeatureGateStatus::Activated { slot } = v.status {
                warnings.push(format!(
                    "Feature is already activated (slot {slot}); activation would be a no-op."
                ));
            }
            if !v.rent_exempt {
                warnings.push(format!(
                    "Feature account is not rent-exempt ({} lamports); activation may not persist.",
                    v.lamports
                ));
            }
        }
        TransactionKind::Revoke => {
            if v.status != FeatureGateStatus::Pending {
                warnings.push(format!(
                    "Revoke requires a pending activation, but feature status is {:?}; execution will fail on-chain.",
                    v.status
                ));
            }
        }
        // Rekey is a config transaction on the multisig itself, not an
        // operation on the feature account, so it has no feature-state warnings.
        TransactionKind::Rekey => {}
    }
    if let FeatureGateStatus::Unexpected { owner, data_len } = v.status {
        warnings.push(format!(
            "Feature account has an unexpected owner ({owner}) / {data_len} bytes."
        ));
    }
    warnings
}

/// Result of verifying the Squads program on the target cluster.
#[derive(Debug, Clone)]
pub struct ProgramAuthenticity {
    pub program_id: Pubkey,
    pub executable: bool,
    pub loader_owner_ok: bool,
    pub immutable: bool,
    pub upgrade_authority: Option<Pubkey>,
    pub on_chain_hash: String,
    pub hash_matches: bool,
}

/// Verify the Squads program at [`SQUADS_MULTISIG_PROGRAM_ID`]: executable,
/// owned by the upgradeable loader, immutable, and bytecode matching the known
/// Squads v4 hash.
pub fn verify_squads_program(rpc: &RpcClient) -> Result<ProgramAuthenticity> {
    let program_id = SQUADS_MULTISIG_PROGRAM_ID;
    let program = rpc
        .get_account(&program_id)
        .map_err(|e| eyre!("failed to fetch Squads program account: {}", e))?;
    let loader_owner_ok = program.owner == BPF_LOADER_UPGRADEABLE_ID;

    let (programdata_address, _) =
        Pubkey::find_program_address(&[program_id.as_ref()], &BPF_LOADER_UPGRADEABLE_ID);
    let programdata = rpc
        .get_account(&programdata_address)
        .map_err(|e| eyre!("failed to fetch Squads programdata account: {}", e))?;

    let (upgrade_authority, elf) = parse_programdata(&programdata.data)?;
    let on_chain_hash = program_hash(elf);

    Ok(ProgramAuthenticity {
        program_id,
        executable: program.executable,
        loader_owner_ok,
        immutable: upgrade_authority.is_none(),
        upgrade_authority,
        hash_matches: on_chain_hash == SQUADS_V4_PROGRAM_HASH,
        on_chain_hash,
    })
}

/// Advisory warnings about the Squads program's authenticity. Never blocks.
///
/// `is_mainnet` gates the immutability and bytecode-hash checks: those only hold
/// on mainnet, where the official program is frozen and verified. Squads keeps
/// its devnet/testnet deployments upgradeable and on different builds, so a
/// mutable authority or mismatched hash there is expected, not a red flag.
pub fn program_warnings(p: &ProgramAuthenticity, is_mainnet: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    if !p.executable {
        warnings.push("Squads program account is not executable.".to_string());
    }
    if !p.loader_owner_ok {
        warnings.push("Squads program is not owned by the upgradeable BPF loader.".to_string());
    }
    if !is_mainnet {
        return warnings;
    }
    if !p.immutable {
        warnings.push(format!(
            "Squads program is mutable (upgrade authority {}); the official program is frozen.",
            p.upgrade_authority
                .map(|a| a.to_string())
                .unwrap_or_default()
        ));
    }
    if !p.hash_matches {
        warnings.push(format!(
            "Squads program bytecode hash {} does not match the known Squads v4 hash {}.",
            p.on_chain_hash, SQUADS_V4_PROGRAM_HASH
        ));
    }
    warnings
}

/// Governance-safety warnings about a feature gate multisig's configuration.
/// Never blocks; the verify command surfaces these.
///
/// A feature gate multisig must be autonomous: its `config_authority` is the
/// default (all-zero) sentinel, so members and threshold can only change by a
/// member vote. Any other authority can rewrite them unilaterally, which makes
/// the owner list meaningless. A non-zero time lock delays executions.
pub fn multisig_safety_warnings(ms: &Multisig) -> Vec<String> {
    let mut warnings = Vec::new();
    if ms.config_authority != Pubkey::default() {
        warnings.push(format!(
            "Multisig is not autonomous: config authority {} can change members and threshold unilaterally, so the owner list is not binding.",
            ms.config_authority
        ));
    }
    if ms.time_lock != 0 {
        warnings.push(format!(
            "Multisig has a non-zero time lock ({}s); executions are delayed by that long after approval.",
            ms.time_lock
        ));
    }
    warnings
}

/// True when the multisig is autonomous (config changes require a member vote).
pub fn is_autonomous(ms: &Multisig) -> bool {
    ms.config_authority == Pubkey::default()
}

/// True when the multisig can never pass another proposal: it is autonomous and
/// its usable voting keys cannot meet the threshold.
///
/// Autonomy is part of the test, not a separate concern: a non-default
/// `config_authority` can add members and change the threshold directly, so an
/// unreachable threshold freezes nothing while such an authority exists. The rekey flow deliberately produces this
/// state (a single `Pubkey::default()` member, which can never sign),
/// permanently freezing the feature gate's configuration.
pub fn is_rekeyed(ms: &Multisig) -> bool {
    if !is_autonomous(ms) {
        return false;
    }
    let usable_voters = ms
        .members
        .iter()
        .filter(|m| {
            m.key != Pubkey::default() && m.permissions.mask & crate::squads::PERMISSION_VOTE != 0
        })
        .count();
    usable_voters < usize::from(ms.threshold)
}

/// The expected feature gate governance signers: each party's stable voting
/// key (an EOA or, more likely, its parent multisig's vault-0 PDA), which
/// recurs as a member across every feature gate multisig.
///
/// Vendored like [`SQUADS_V4_PROGRAM_HASH`]: expectations live in reviewed
/// code, not editable config. Fill in once the parties' keys are known, e.g.:
///
/// ```text
/// ("Org A", Pubkey::from_str_const("...")),
/// ("Org B", Pubkey::from_str_const("...")),
/// ("Org C", Pubkey::from_str_const("...")),
/// ```
///
/// While empty, the member-set check is skipped.
pub const KNOWN_SIGNERS: &[(&str, Pubkey)] = &[];

/// The vendored name for a known governance signer key, if any.
pub fn known_signer_name(key: &Pubkey) -> Option<&'static str> {
    KNOWN_SIGNERS
        .iter()
        .find(|(_, known)| known == key)
        .map(|(name, _)| *name)
}

/// Warnings when the multisig's voting members differ from the vendored
/// expected signer set. Empty when [`KNOWN_SIGNERS`] is empty. Initiate-only
/// members (the ephemeral contributor key) are ignored: only keys that can
/// vote matter for governance.
pub fn member_set_warnings(ms: &Multisig) -> Vec<String> {
    member_set_warnings_against(ms, KNOWN_SIGNERS)
}

/// An expected governance signer: a display name plus its key. The key is
/// `None` when a configured entry is not a valid public key, so it can never
/// match a member (in particular not the `Pubkey::default()` member a rekey
/// leaves behind).
pub type ExpectedSigner = (String, Option<Pubkey>);

/// The signer set to hold a multisig to, and a phrase naming its source:
/// [`KNOWN_SIGNERS`] when vendored into this build, otherwise the operator's
/// configured member list. `None` when there is nothing to compare against.
/// An unparseable configured entry keeps its raw text with no key, so it is
/// reported as broken rather than quietly shrinking the check.
pub fn expected_signers(config_members: &[String]) -> Option<(Vec<ExpectedSigner>, &'static str)> {
    if !KNOWN_SIGNERS.is_empty() {
        let vendored = KNOWN_SIGNERS
            .iter()
            .map(|(name, key)| ((*name).to_string(), Some(*key)))
            .collect();
        return Some((vendored, "the signer set vendored into this build"));
    }
    if config_members.is_empty() {
        return None;
    }
    let configured = config_members
        .iter()
        .map(|entry| (entry.clone(), Pubkey::from_str(entry).ok()))
        .collect();
    Some((configured, "your configured member list"))
}

/// Warnings when the multisig's voting members differ from `expected`.
/// Initiate-only members (the ephemeral contributor key) are ignored: only
/// keys that can vote or execute matter for governance.
pub fn member_set_warnings_for(ms: &Multisig, expected: &[ExpectedSigner]) -> Vec<String> {
    use crate::squads::{PERMISSION_EXECUTE, PERMISSION_VOTE};

    if expected.is_empty() {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    for (name, key) in expected {
        let is_voting_member = key.is_some_and(|key| {
            ms.members
                .iter()
                .any(|m| m.key == key && m.permissions.mask & PERMISSION_VOTE != 0)
        });
        if is_voting_member {
            continue;
        }
        warnings.push(match key {
            // A broken entry can never match any member; say so instead of
            // implying the signer is merely absent.
            None => format!(
                "Expected signer {name} is not a valid public key; fix this entry in your config."
            ),
            // Configured sets use the key as the name; don't print it twice.
            Some(key) if *name == key.to_string() => {
                format!("Expected signer {key} is not a voting member of this multisig.")
            }
            Some(key) => {
                format!("Expected signer {name} ({key}) is not a voting member of this multisig.")
            }
        });
    }

    // Any extra member that can vote or execute is a party with power over
    // governance; only pure Initiate-only members (the expected contributor
    // pattern) are exempt.
    for member in &ms.members {
        if member.permissions.mask & (PERMISSION_VOTE | PERMISSION_EXECUTE) == 0 {
            continue;
        }
        if expected.iter().any(|(_, k)| *k == Some(member.key)) {
            continue;
        }
        let abilities = match (
            member.permissions.mask & PERMISSION_VOTE != 0,
            member.permissions.mask & PERMISSION_EXECUTE != 0,
        ) {
            (true, true) => "vote and execute",
            (true, false) => "vote",
            (false, _) => "execute proposals",
        };
        warnings.push(format!(
            "Member {} can {abilities} but is not one of the expected governance signers.",
            member.key
        ));
    }
    warnings
}

fn member_set_warnings_against<N: AsRef<str>>(ms: &Multisig, known: &[(N, Pubkey)]) -> Vec<String> {
    use crate::squads::{PERMISSION_EXECUTE, PERMISSION_VOTE};

    if known.is_empty() {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    for (name, key) in known {
        let is_voting_member = ms
            .members
            .iter()
            .any(|m| m.key == *key && m.permissions.mask & PERMISSION_VOTE != 0);
        if !is_voting_member {
            let name = name.as_ref();
            // Configured sets use the key as the name; don't print it twice.
            if name == key.to_string() {
                warnings.push(format!(
                    "Expected signer {key} is not a voting member of this multisig."
                ));
            } else {
                warnings.push(format!(
                    "Expected signer {name} ({key}) is not a voting member of this multisig."
                ));
            }
        }
    }

    // Any extra member that can vote or execute is a party with power over
    // governance; only pure Initiate-only members (the expected contributor
    // pattern) are exempt.
    for member in &ms.members {
        if member.permissions.mask & (PERMISSION_VOTE | PERMISSION_EXECUTE) == 0 {
            continue;
        }
        if known.iter().any(|(_, k)| k == &member.key) {
            continue;
        }
        let abilities = match (
            member.permissions.mask & PERMISSION_VOTE != 0,
            member.permissions.mask & PERMISSION_EXECUTE != 0,
        ) {
            (true, true) => "vote and execute",
            (true, false) => "vote",
            (false, _) => "execute proposals",
        };
        warnings.push(format!(
            "Member {} can {abilities} but is not one of the expected governance signers.",
            member.key
        ));
    }
    warnings
}

/// A canonical fingerprint of a multisig's governance-relevant config, for
/// detecting drift across networks. Covers threshold, config authority, time
/// lock, and the member set (key + permissions), order-independent.
pub fn config_fingerprint(ms: &Multisig) -> String {
    let mut members: Vec<String> = ms
        .members
        .iter()
        .map(|m| format!("{}:{}", m.key, m.permissions.mask))
        .collect();
    members.sort();
    format!(
        "threshold={};authority={};time_lock={};members=[{}]",
        ms.threshold,
        ms.config_authority,
        ms.time_lock,
        members.join(",")
    )
}

/// Parse an upgradeable-loader ProgramData account into its upgrade authority
/// and the ELF that follows the fixed-size metadata header.
fn parse_programdata(data: &[u8]) -> Result<(Option<Pubkey>, &[u8])> {
    if data.len() < PROGRAMDATA_HEADER_LEN {
        return Err(eyre!(
            "programdata account too small: {} bytes (expected at least {})",
            data.len(),
            PROGRAMDATA_HEADER_LEN
        ));
    }
    // Layout: [0..4] enum tag (3 = ProgramData), [4..12] slot, [12] Option tag,
    // [13..45] authority pubkey (meaningful only when the Option tag is 1).
    if data[0..4] != PROGRAMDATA_ENUM_TAG {
        return Err(eyre!(
            "account is not a ProgramData record (enum tag {:?})",
            &data[0..4]
        ));
    }
    // Only 0 and 1 are valid Option tags. Treating anything else as None
    // reported a malformed record as an immutable program.
    let upgrade_authority = match data[12] {
        0 => None,
        1 => Some(Pubkey::new_from_array(
            data[13..45].try_into().unwrap_or([0u8; 32]),
        )),
        tag => {
            return Err(eyre!(
                "programdata upgrade-authority option tag is {tag}, expected 0 or 1"
            ))
        }
    };
    Ok((upgrade_authority, &data[PROGRAMDATA_HEADER_LEN..]))
}

/// Hash a program ELF the way `solana-verify` does: sha256 over the bytes with
/// trailing zero padding removed.
fn program_hash(elf: &[u8]) -> String {
    let trimmed = trim_trailing_zeros(elf);
    hex_lower(&solana_sha256_hasher::hash(trimmed).to_bytes())
}

fn trim_trailing_zeros(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    &bytes[..end]
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn account_not_found(msg: &str) -> bool {
    msg.contains("AccountNotFound") || msg.contains("could not find account")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_feature_account_states() {
        let sys = solana_system_interface::program::ID;
        assert_eq!(
            classify_feature_account(&sys, &[]),
            FeatureGateStatus::Fresh
        );

        // Pending: Feature-owned, option tag 0.
        let pending = [0u8; FEATURE_ACCOUNT_SIZE];
        assert_eq!(
            classify_feature_account(&FEATURE_GATE_PROGRAM_ID, &pending),
            FeatureGateStatus::Pending
        );

        // Activated: option tag 1 + slot.
        let mut activated = [0u8; FEATURE_ACCOUNT_SIZE];
        activated[0] = 1;
        activated[1..9].copy_from_slice(&42u64.to_le_bytes());
        assert_eq!(
            classify_feature_account(&FEATURE_GATE_PROGRAM_ID, &activated),
            FeatureGateStatus::Activated { slot: 42 }
        );

        // Wrong owner is unexpected.
        let other = Pubkey::new_unique();
        assert!(matches!(
            classify_feature_account(&other, &[0u8; 9]),
            FeatureGateStatus::Unexpected { .. }
        ));
    }

    #[test]
    fn program_hash_trims_trailing_zeros() {
        // Hashing trims trailing zeros, so padding does not change the result.
        let core = [1u8, 2, 3, 4];
        let mut padded = core.to_vec();
        padded.extend_from_slice(&[0u8; 16]);
        assert_eq!(program_hash(&core), program_hash(&padded));
    }

    fn verification_with(status: FeatureGateStatus, rent_exempt: bool) -> FeatureVerification {
        FeatureVerification {
            feature_id: Pubkey::new_unique(),
            status,
            lamports: 0,
            rent_exempt,
        }
    }

    #[test]
    fn feature_action_warnings_match_status() {
        // Activate on an already-activated feature warns about the no-op.
        let v = verification_with(FeatureGateStatus::Activated { slot: 1 }, true);
        assert!(feature_action_warnings(&v, TransactionKind::Activate)
            .iter()
            .any(|w| w.contains("already activated")));

        // Activate on a fresh, non-rent-exempt account warns about persistence.
        let v = verification_with(FeatureGateStatus::Fresh, false);
        assert!(feature_action_warnings(&v, TransactionKind::Activate)
            .iter()
            .any(|w| w.contains("rent-exempt")));

        // Revoke is fine on Pending, warns otherwise.
        let pending = verification_with(FeatureGateStatus::Pending, true);
        assert!(feature_action_warnings(&pending, TransactionKind::Revoke).is_empty());
        let activated = verification_with(FeatureGateStatus::Activated { slot: 1 }, true);
        assert!(!feature_action_warnings(&activated, TransactionKind::Revoke).is_empty());

        // Rekey is not a feature-account op: no feature warnings regardless of state.
        assert!(feature_action_warnings(&pending, TransactionKind::Rekey).is_empty());
    }

    #[test]
    fn program_warnings_gate_immutability_and_hash_to_mainnet() {
        // A mutable, mismatched-hash program: a red flag on mainnet, expected elsewhere.
        let p = ProgramAuthenticity {
            program_id: SQUADS_MULTISIG_PROGRAM_ID,
            executable: true,
            loader_owner_ok: true,
            immutable: false,
            upgrade_authority: Some(Pubkey::new_unique()),
            on_chain_hash: "deadbeef".to_string(),
            hash_matches: false,
        };

        let mainnet = program_warnings(&p, true);
        assert!(mainnet.iter().any(|w| w.contains("mutable")));
        assert!(mainnet.iter().any(|w| w.contains("does not match")));

        // Off mainnet, neither the mutability nor the hash mismatch warns.
        assert!(program_warnings(&p, false).is_empty());

        // Core checks (executable, loader ownership) still fire on any network.
        let broken = ProgramAuthenticity {
            executable: false,
            loader_owner_ok: false,
            ..p
        };
        assert_eq!(program_warnings(&broken, false).len(), 2);
    }

    fn multisig_with(config_authority: Pubkey, time_lock: u32, threshold: u16) -> Multisig {
        use crate::squads::{Member, Permissions};
        Multisig {
            create_key: Pubkey::new_unique(),
            config_authority,
            threshold,
            time_lock,
            transaction_index: 0,
            stale_transaction_index: 0,
            rent_collector: None,
            bump: 0,
            members: vec![Member {
                key: Pubkey::from_str_const("11111111111111111111111111111112"),
                permissions: Permissions::all(),
            }],
        }
    }

    #[test]
    fn multisig_safety_warns_on_authority_and_time_lock() {
        // Autonomous, no time lock: clean.
        let safe = multisig_with(Pubkey::default(), 0, 2);
        assert!(multisig_safety_warnings(&safe).is_empty());
        assert!(is_autonomous(&safe));

        // A real config authority makes the owner list non-binding.
        let controlled = multisig_with(Pubkey::new_unique(), 0, 2);
        assert!(!is_autonomous(&controlled));
        assert!(multisig_safety_warnings(&controlled)
            .iter()
            .any(|w| w.contains("not autonomous")));

        // Non-zero time lock warns.
        let delayed = multisig_with(Pubkey::default(), 3600, 2);
        assert!(multisig_safety_warnings(&delayed)
            .iter()
            .any(|w| w.contains("time lock")));
    }

    #[test]
    fn detects_rekeyed_multisigs() {
        use crate::squads::{Member, Permissions};

        // Healthy: one real voter, threshold 1.
        let healthy = multisig_with(Pubkey::default(), 0, 1);
        assert!(!is_rekeyed(&healthy));

        // Canonical rekey shape: only the unsignable default-pubkey member.
        let mut rekeyed = multisig_with(Pubkey::default(), 0, 1);
        rekeyed.members = vec![Member {
            key: Pubkey::default(),
            permissions: Permissions::all(),
        }];
        assert!(is_rekeyed(&rekeyed));

        // The same unusable member set, but a config authority can add members
        // and move the threshold directly, so nothing is frozen. Claiming
        // "permanently frozen" here would be false.
        let mut authority_held = rekeyed.clone();
        authority_held.config_authority = Pubkey::new_unique();
        assert!(
            !is_rekeyed(&authority_held),
            "a unilateral config authority can restore quorum, so this is not frozen"
        );

        // Quorum impossible more generally: threshold above the usable voters.
        let stuck = multisig_with(Pubkey::default(), 0, 3);
        assert!(is_rekeyed(&stuck));
    }

    #[test]
    fn member_set_warnings_compare_voting_members_only() {
        use crate::squads::{Member, Permissions};
        let org_a = Pubkey::new_unique();
        let org_b = Pubkey::new_unique();
        let stranger = Pubkey::new_unique();
        let contributor = Pubkey::new_unique();
        let known = [("Org A", org_a), ("Org B", org_b)];

        let mut ms = multisig_with(Pubkey::default(), 0, 2);
        ms.members = vec![
            Member {
                key: org_a,
                permissions: Permissions::all(),
            },
            Member {
                key: org_b,
                permissions: Permissions::all(),
            },
            // Initiate-only contributor must not trip the check.
            Member {
                key: contributor,
                permissions: Permissions { mask: 1 },
            },
        ];
        assert!(member_set_warnings_against(&ms, &known).is_empty());

        // A missing expected signer and an unexpected voter both warn.
        ms.members[1].key = stranger;
        let warnings = member_set_warnings_against(&ms, &known);
        assert!(warnings
            .iter()
            .any(|w| w.contains("Org B") && w.contains("not a voting member")));
        assert!(warnings
            .iter()
            .any(|w| w.contains(&stranger.to_string()) && w.contains("not one of the expected")));

        // An unexpected executor (no vote) warns too; extra powers over
        // governance are not limited to voting.
        ms.members[1] = Member {
            key: org_b,
            permissions: Permissions::all(),
        };
        ms.members.push(Member {
            key: stranger,
            permissions: Permissions { mask: 4 },
        });
        let warnings = member_set_warnings_against(&ms, &known);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("execute proposals"));

        // Empty registry (the current vendored state): check is skipped.
        assert!(member_set_warnings_against(&ms, &[] as &[(&str, Pubkey)]).is_empty());
    }

    #[test]
    fn unparseable_config_entry_never_matches_a_member() {
        use crate::squads::{Member, Permissions};

        // The rekeyed shape: a default-pubkey voting member. A typo'd config
        // entry used to parse to Pubkey::default() and silently match it,
        // suppressing the missing-signer warning exactly where it mattered.
        let mut ms = multisig_with(Pubkey::default(), 0, 1);
        ms.members = vec![Member {
            key: Pubkey::default(),
            permissions: Permissions::all(),
        }];

        let (expected, source) = expected_signers(&["not-a-pubkey".to_string()]).unwrap();
        assert_eq!(source, "your configured member list");
        assert_eq!(expected, vec![("not-a-pubkey".to_string(), None)]);

        let warnings = member_set_warnings_for(&ms, &expected);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("not-a-pubkey") && w.contains("not a valid public key")),
            "{warnings:?}"
        );
    }

    #[test]
    fn config_fingerprint_ignores_member_order_and_tracks_threshold() {
        use crate::squads::{Member, Permissions};
        let mut a = multisig_with(Pubkey::default(), 0, 2);
        let mut b = a.clone();
        // Same members, different declared order -> identical fingerprint.
        a.members = vec![
            Member {
                key: Pubkey::from_str_const("11111111111111111111111111111112"),
                permissions: Permissions::all(),
            },
            Member {
                key: Pubkey::from_str_const("11111111111111111111111111111113"),
                permissions: Permissions::all(),
            },
        ];
        b.members = vec![a.members[1].clone(), a.members[0].clone()];
        assert_eq!(config_fingerprint(&a), config_fingerprint(&b));

        // Threshold change -> different fingerprint.
        b.threshold = 1;
        assert_ne!(config_fingerprint(&a), config_fingerprint(&b));
    }

    /// An endpoint may escalate strictness but never relax it. Getting this
    /// backwards - requiring the URL and the chain to agree - breaks every
    /// mainnet fork on a custom URL, which is how this was first written.
    #[test]
    fn cluster_strictness_can_be_escalated_but_not_relaxed() {
        // Mainnet fork on a custom URL: chain says mainnet, URL does not. Strict.
        assert_eq!(
            strictness_for(Some(true), false, "http://127.0.0.1:8899").unwrap(),
            (true, true)
        );
        // Both agree, either way.
        assert_eq!(
            strictness_for(Some(true), true, "https://api.mainnet.solana.com").unwrap(),
            (true, true)
        );
        assert_eq!(
            strictness_for(Some(false), false, "https://api.devnet.solana.com").unwrap(),
            (false, true)
        );
        // The downgrade attempt: URL names mainnet, chain disagrees. Refused.
        let err = strictness_for(Some(false), true, "https://api.mainnet.solana.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Cluster mismatch"), "got: {err}");

        // Chain unreachable: fall back to the URL and report it unverified.
        assert_eq!(
            strictness_for(None, true, "https://api.mainnet.solana.com").unwrap(),
            (true, false)
        );
        assert_eq!(
            strictness_for(None, false, "http://127.0.0.1:8899").unwrap(),
            (false, false)
        );
    }

    #[test]
    fn parses_programdata_authority() {
        // tag 0 -> immutable (None)
        let mut immutable = vec![0u8; PROGRAMDATA_HEADER_LEN + 4];
        immutable[0] = 3;
        let (auth, elf) = parse_programdata(&immutable).unwrap();
        assert_eq!(auth, None);
        assert_eq!(elf.len(), 4);

        // tag 1 -> Some(authority)
        let mut mutable = vec![0u8; PROGRAMDATA_HEADER_LEN];
        mutable[0] = 3;
        mutable[12] = 1;
        let key = Pubkey::new_unique();
        mutable[13..45].copy_from_slice(key.as_ref());
        let (auth, _) = parse_programdata(&mutable).unwrap();
        assert_eq!(auth, Some(key));
    }

    /// Malformed bytes must not be read as an immutable program: "no upgrade
    /// authority" is the reassuring answer, so it has to come from a record that
    /// actually says so.
    #[test]
    fn malformed_programdata_is_rejected_not_read_as_immutable() {
        // Wrong enum tag: some other upgradeable-loader account.
        let mut wrong_kind = vec![0u8; PROGRAMDATA_HEADER_LEN];
        wrong_kind[0] = 2;
        let err = parse_programdata(&wrong_kind).unwrap_err().to_string();
        assert!(err.contains("not a ProgramData record"), "got: {err}");

        // Option tag outside {0, 1}: previously fell through to None.
        let mut bad_option = vec![0u8; PROGRAMDATA_HEADER_LEN];
        bad_option[0] = 3;
        bad_option[12] = 7;
        let err = parse_programdata(&bad_option).unwrap_err().to_string();
        assert!(err.contains("option tag is 7"), "got: {err}");
    }
}
