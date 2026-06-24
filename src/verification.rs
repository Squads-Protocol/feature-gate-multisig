//! Verification helpers for vetting a feature gate before acting on it.
//!
//! Two independent dimensions:
//! - Feature gate validity: the on-chain state of the feature account (the
//!   multisig's vault PDA) and whether it matches the action being taken.
//! - Squads program authenticity: that the program at [`SQUADS_MULTISIG_PROGRAM_ID`]
//!   on the target cluster is the genuine, immutable Squads v4 program.

use crate::commands::TransactionKind;
use crate::feature_gate_program::{FEATURE_ACCOUNT_SIZE, FEATURE_GATE_PROGRAM_ID};
use crate::squads::{get_vault_pda, SQUADS_MULTISIG_PROGRAM_ID};
use eyre::{eyre, Result};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;

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
    pub multisig: Pubkey,
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
        Ok(acc) => (classify_feature_account(&acc.owner, &acc.data), acc.lamports),
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
        multisig: *multisig,
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
pub fn program_warnings(p: &ProgramAuthenticity) -> Vec<String> {
    let mut warnings = Vec::new();
    if !p.executable {
        warnings.push("Squads program account is not executable.".to_string());
    }
    if !p.loader_owner_ok {
        warnings.push("Squads program is not owned by the upgradeable BPF loader.".to_string());
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
    let upgrade_authority = match data[12] {
        1 => Some(Pubkey::new_from_array(
            data[13..45].try_into().unwrap_or([0u8; 32]),
        )),
        _ => None,
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
            multisig: Pubkey::new_unique(),
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
}
