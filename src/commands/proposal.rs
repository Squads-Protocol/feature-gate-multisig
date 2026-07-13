//! Non-interactive proposal subcommands: `propose`, `approve`, `reject`,
//! `execute`. Thin argument-resolution wrappers over the same flow functions
//! the interactive menu uses, so signers can act from a runbook one-liner.

use crate::commands::transaction_generation::{
    approve_common_config_change, approve_common_feature_gate_proposal,
    build_config_actions_for_kind, create_feature_gate_proposal, execute_common_config_change,
    execute_common_feature_gate_proposal, reject_common_feature_gate_proposal,
    rekey_multisig_feature_gate, TransactionKind,
};
use crate::feature_gate_program::{activate_feature_funded, revoke_pending_activation};
use crate::output::Output;
use crate::provision::{create_rpc_client, fetch_squads_multisig};
use crate::squads::{
    deserialize_squads_account, get_transaction_pda, get_vault_pda, ConfigTransaction, Member,
    ProposalStatus, VaultTransaction, VaultTransactionMessage,
    CONFIG_TRANSACTION_ACCOUNT_DISCRIMINATOR, SQUADS_MULTISIG_PROGRAM_ID,
    VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
};
use crate::utils::{
    choose_network_from_config, is_assume_yes, is_e2e_test_mode, prompt_for_fee_payer_path,
    prompt_for_pubkey, save_config, Config,
};
use eyre::Result;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use std::str::FromStr;

/// Which proposal action a subcommand performs.
#[derive(Debug, Clone, Copy)]
pub enum ProposalCommand {
    Propose,
    Approve,
    Reject,
    Execute,
}

/// Resolved inputs shared by every proposal subcommand.
pub struct ProposalCommandArgs {
    pub multisig: String,
    pub kind: TransactionKind,
    /// Voting key flag; falls back to the config default, then to a prompt.
    pub voting_key: Option<String>,
    /// Fee payer keypair path flag; falls back to the config default, then to a prompt.
    pub keypair: Option<String>,
    /// Proposal index (required for approve/reject/execute; unused for propose).
    pub index: Option<u64>,
}

pub fn proposal_command(
    config: &Config,
    command: ProposalCommand,
    args: ProposalCommandArgs,
) -> Result<()> {
    let multisig = Pubkey::from_str(&args.multisig)
        .map_err(|_| eyre::eyre!("Invalid multisig address: {}", args.multisig))?;

    // Resolve the acting network once (persistence uses the original `config`;
    // dispatch uses a single-network clone so downstream never re-prompts).
    let rpc_url = choose_network_from_config(config)?;
    let voting_key = resolve_voting_key(config, args.voting_key.as_deref())?;
    let fee_payer_path = match args.keypair {
        Some(path) => path,
        None => prompt_for_fee_payer_path(config)?,
    };
    let dispatch_config = Config {
        networks: vec![rpc_url.clone()],
        ..config.clone()
    };
    let config = &dispatch_config;

    // Acting on an existing proposal: confirm the on-chain transaction actually
    // matches the kind the caller specified, so an approve/reject/execute can
    // never be applied to a different (or disguised) transaction than intended.
    if let Some(idx) = args.index {
        reconcile_requested_kind(&create_rpc_client(&rpc_url), &multisig, idx, args.kind)?;
    }

    let index = || {
        args.index.ok_or_else(|| {
            eyre::eyre!("--index is required: the proposal index to act on (see `show`)")
        })
    };

    match (command, args.kind) {
        (ProposalCommand::Propose, TransactionKind::Rekey) => {
            rekey_multisig_feature_gate(config, multisig, voting_key, fee_payer_path)
        }
        (ProposalCommand::Propose, kind) => {
            create_feature_gate_proposal(config, multisig, voting_key, fee_payer_path, kind)
        }
        (ProposalCommand::Approve, TransactionKind::Rekey) => {
            approve_common_config_change(config, multisig, voting_key, fee_payer_path, index()?)
        }
        (ProposalCommand::Approve, kind) => approve_common_feature_gate_proposal(
            config,
            multisig,
            voting_key,
            fee_payer_path,
            index()?,
            kind,
        ),
        (ProposalCommand::Reject, kind) => reject_common_feature_gate_proposal(
            config,
            multisig,
            voting_key,
            fee_payer_path,
            index()?,
            kind,
        ),
        (ProposalCommand::Execute, TransactionKind::Rekey) => {
            execute_common_config_change(config, multisig, voting_key, fee_payer_path, index()?)
        }
        (ProposalCommand::Execute, kind) => execute_common_feature_gate_proposal(
            config,
            multisig,
            voting_key,
            fee_payer_path,
            index()?,
            kind,
        ),
    }
}

/// Guard an approve/reject/execute against acting on a mismatched or disguised
/// transaction. The on-chain transaction at `index` is classified structurally
/// (see [`classify_vault_message`]):
/// - a recognized kind that differs from `requested` is a hard error;
/// - an unrecognized transaction (one this tool did not build) is not blocked -
///   another Squads client may have created it - but the user is warned that its
///   contents can't be verified and pointed at the full `show --index` view.
fn reconcile_requested_kind(
    rpc_client: &RpcClient,
    multisig: &Pubkey,
    index: u64,
    requested: TransactionKind,
) -> Result<()> {
    let on_chain = describe_transaction(rpc_client, multisig, index);
    match on_chain.transaction_kind() {
        Some(actual) if actual == requested => Ok(()),
        Some(actual) => Err(eyre::eyre!(
            "Proposal #{index} is a {} on-chain, but you specified {}. Refusing to act on a different transaction than intended.",
            actual.label(),
            requested.label(),
        )),
        None => {
            Output::warning(&format!(
                "Proposal #{index} is \"{}\": this tool cannot verify what it does from its on-chain instructions. Inspect it with `show <multisig> --index {index}` and only proceed if you independently trust it.",
                on_chain.label()
            ));
            Ok(())
        }
    }
}

/// Resolve the voting key: explicit flag, then the config default, then a prompt.
/// A key obtained interactively is offered to be saved, since a signer's identity
/// is stable across the many feature gate multisigs they act on.
fn resolve_voting_key(config: &Config, flag: Option<&str>) -> Result<Pubkey> {
    if let Some(key) = flag {
        return Pubkey::from_str(key).map_err(|_| eyre::eyre!("Invalid voting key: {}", key));
    }
    if let Some(saved) = &config.voting_key {
        let key = Pubkey::from_str(saved)
            .map_err(|_| eyre::eyre!("Saved voting key in config is invalid: {}", saved))?;
        Output::info(&format!("Using saved voting key: {key}"));
        return Ok(key);
    }

    let key = prompt_for_pubkey("Enter the voting key (EOA or parent multisig):")?;
    if !is_e2e_test_mode()
        && !is_assume_yes()
        && inquire::Confirm::new("Save as default voting key for future commands?")
            .with_default(true)
            .prompt()
            .unwrap_or(false)
    {
        let mut updated = config.clone();
        updated.voting_key = Some(key.to_string());
        save_config(&updated)?;
        Output::success("Voting key saved to config.");
    }
    Ok(key)
}

/// What a proposal's companion transaction does, classified from its on-chain
/// shape. The tool only creates three forms: activate (System-program
/// allocate+assign), revoke (an instruction to the Feature Gate program), and
/// rekey (a config transaction).
#[derive(Clone, Copy)]
pub(crate) enum ProposalKind {
    Activate,
    Revoke,
    Rekey,
    ChildAction,
    Other,
    Unknown,
}

impl ProposalKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ProposalKind::Activate => "Activate feature gate",
            ProposalKind::Revoke => "Revoke feature gate",
            ProposalKind::Rekey => "Rekey (config change)",
            ProposalKind::ChildAction => "Child multisig action",
            ProposalKind::Other => "Vault transaction",
            ProposalKind::Unknown => "Unknown",
        }
    }

    pub(crate) fn transaction_kind(self) -> Option<TransactionKind> {
        match self {
            ProposalKind::Activate => Some(TransactionKind::Activate),
            ProposalKind::Revoke => Some(TransactionKind::Revoke),
            ProposalKind::Rekey => Some(TransactionKind::Rekey),
            ProposalKind::ChildAction | ProposalKind::Other | ProposalKind::Unknown => None,
        }
    }
}

pub(crate) fn describe_transaction(
    rpc_client: &RpcClient,
    multisig: &Pubkey,
    index: u64,
) -> ProposalKind {
    let (transaction_pda, _) = get_transaction_pda(multisig, index);
    let Ok(account) = rpc_client.get_account(&transaction_pda) else {
        return ProposalKind::Unknown;
    };

    if let Ok(config_tx) = deserialize_squads_account::<ConfigTransaction>(
        &account.data,
        CONFIG_TRANSACTION_ACCOUNT_DISCRIMINATOR,
        "config transaction",
    ) {
        // Classify a config transaction as Rekey only when its actions are the
        // canonical brick set, not merely because it is a config transaction.
        // A config change that adds a member or lowers the threshold must not
        // wear the benign "Rekey" label. Compared against the current members,
        // which equal the rekey's targets until it executes.
        let members = fetch_squads_multisig(rpc_client, multisig, "multisig")
            .map(|ms| ms.members)
            .unwrap_or_default();
        return if is_canonical_rekey(&config_tx.actions, &members) {
            ProposalKind::Rekey
        } else {
            ProposalKind::Other
        };
    }

    let Ok(vault_tx) = deserialize_squads_account::<VaultTransaction>(
        &account.data,
        VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
        "vault transaction",
    ) else {
        return ProposalKind::Unknown;
    };

    classify_vault_message(&vault_tx.message, multisig)
}

/// True when a config transaction's actions are exactly the canonical rekey
/// brick set for `members` (add the unsignable default member, drop the
/// threshold to 1, remove every current member). Any other config change - an
/// added member, a lowered threshold on its own - is not a rekey and must not
/// be labeled one.
fn is_canonical_rekey(actions: &[crate::squads::ConfigAction], members: &[Member]) -> bool {
    match build_config_actions_for_kind(TransactionKind::Rekey, members) {
        Ok(expected) => actions == expected.as_slice(),
        Err(_) => false,
    }
}

/// Program id, ordered account pubkeys, and raw data of one instruction - the
/// fields that fully determine what it executes.
type InstructionShape = (Pubkey, Vec<Pubkey>, Vec<u8>);

/// Resolve a Squads compiled message into per-instruction shapes, mapping each
/// program/account index back to its pubkey. Returns None if any index is out
/// of range (malformed message).
fn resolved_shapes(message: &VaultTransactionMessage) -> Option<Vec<InstructionShape>> {
    message
        .instructions
        .iter()
        .map(|ix| {
            let program = *message.account_keys.get(ix.program_id_index as usize)?;
            let accounts = ix
                .account_indexes
                .iter()
                .map(|&i| message.account_keys.get(i as usize).copied())
                .collect::<Option<Vec<_>>>()?;
            Some((program, accounts, ix.data.clone()))
        })
        .collect()
}

/// The shapes of a set of freshly built instructions, for exact comparison.
fn reference_shapes(instructions: &[Instruction]) -> Vec<InstructionShape> {
    instructions
        .iter()
        .map(|ix| {
            (
                ix.program_id,
                ix.accounts.iter().map(|a| a.pubkey).collect(),
                ix.data.clone(),
            )
        })
        .collect()
}

/// Classify a vault transaction by matching its exact instruction structure
/// against what this tool builds, not merely which programs it references.
///
/// `Activate` and `Revoke` map to a concrete [`TransactionKind`] and drive the
/// reassuring label a signer approves on, so they require a byte-exact match to
/// the canonical activate/revoke instructions (including the `assign` target
/// owner and the target account, which live in instruction data/accounts). A
/// look-alike - e.g. a pure-System transaction that assigns the feature account
/// to an attacker program or drains it - therefore classifies as `Other`, never
/// `Activate`.
fn classify_vault_message(message: &VaultTransactionMessage, multisig: &Pubkey) -> ProposalKind {
    let Some(shapes) = resolved_shapes(message) else {
        return ProposalKind::Unknown;
    };
    let feature_gate = get_vault_pda(multisig, 0).0;

    if shapes == reference_shapes(&activate_feature_funded(&feature_gate)) {
        ProposalKind::Activate
    } else if shapes == reference_shapes(&[revoke_pending_activation(&feature_gate)]) {
        ProposalKind::Revoke
    } else if shapes
        .iter()
        .any(|(program, _, _)| *program == SQUADS_MULTISIG_PROGRAM_ID)
    {
        ProposalKind::ChildAction
    } else {
        ProposalKind::Other
    }
}

pub(crate) fn proposal_status_label(status: &ProposalStatus) -> &'static str {
    match status {
        ProposalStatus::Draft { .. } => "Draft",
        ProposalStatus::Active { .. } => "Active",
        ProposalStatus::Rejected { .. } => "Rejected",
        ProposalStatus::Approved { .. } => "Approved",
        ProposalStatus::Executing => "Executing",
        ProposalStatus::Executed { .. } => "Executed",
        ProposalStatus::Cancelled { .. } => "Cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::squads::MultisigCompiledInstruction;
    use solana_system_interface::instruction::{allocate, assign, transfer};

    /// Compile instructions into a Squads vault message the way the on-chain
    /// message stores them: a deduped account-key table with per-instruction
    /// index references. Mirrors what `describe_transaction` reads back.
    fn compile(instructions: &[Instruction]) -> VaultTransactionMessage {
        fn position(key: Pubkey, keys: &mut Vec<Pubkey>) -> u8 {
            match keys.iter().position(|k| *k == key) {
                Some(i) => i as u8,
                None => {
                    keys.push(key);
                    (keys.len() - 1) as u8
                }
            }
        }
        let mut account_keys = Vec::new();
        let mut compiled = Vec::new();
        for ix in instructions {
            let account_indexes = ix
                .accounts
                .iter()
                .map(|a| position(a.pubkey, &mut account_keys))
                .collect();
            let program_id_index = position(ix.program_id, &mut account_keys);
            compiled.push(MultisigCompiledInstruction {
                program_id_index,
                account_indexes,
                data: ix.data.clone(),
            });
        }
        VaultTransactionMessage {
            num_signers: 1,
            num_writable_signers: 1,
            num_writable_non_signers: 0,
            account_keys,
            instructions: compiled,
            address_table_lookups: Vec::new(),
        }
    }

    fn setup() -> (Pubkey, Pubkey) {
        let multisig = Pubkey::new_unique();
        let feature_gate = get_vault_pda(&multisig, 0).0;
        (multisig, feature_gate)
    }

    #[test]
    fn classifies_genuine_activate_and_revoke() {
        let (multisig, feature_gate) = setup();

        let activate = compile(&activate_feature_funded(&feature_gate));
        assert!(matches!(
            classify_vault_message(&activate, &multisig),
            ProposalKind::Activate
        ));

        let revoke = compile(&[revoke_pending_activation(&feature_gate)]);
        assert!(matches!(
            classify_vault_message(&revoke, &multisig),
            ProposalKind::Revoke
        ));
    }

    #[test]
    fn disguised_system_transactions_are_not_activate() {
        let (multisig, feature_gate) = setup();
        let attacker = Pubkey::new_unique();

        // The core exploit: allocate + assign the feature account to an attacker
        // program instead of Feature111. Pure System, so the old program-id-only
        // classifier labeled it "Activate"; it must now be Other.
        let hijack = compile(&[allocate(&feature_gate, 9), assign(&feature_gate, &attacker)]);
        assert!(matches!(
            classify_vault_message(&hijack, &multisig),
            ProposalKind::Other
        ));

        // Draining the feature account is also pure System, also not an activate.
        let drain = compile(&[transfer(&feature_gate, &attacker, 1_000_000)]);
        assert!(matches!(
            classify_vault_message(&drain, &multisig),
            ProposalKind::Other
        ));

        // An extra hidden instruction after the real activate breaks the match.
        let mut tampered = activate_feature_funded(&feature_gate);
        tampered.push(transfer(&feature_gate, &attacker, 1));
        assert!(matches!(
            classify_vault_message(&compile(&tampered), &multisig),
            ProposalKind::Other
        ));
    }

    #[test]
    fn activate_shape_must_target_the_feature_gate() {
        let (multisig, _feature_gate) = setup();
        let other_account = Pubkey::new_unique();

        // Correct instructions but against the wrong account: not this
        // multisig's activation.
        let wrong_target = compile(&activate_feature_funded(&other_account));
        assert!(matches!(
            classify_vault_message(&wrong_target, &multisig),
            ProposalKind::Other
        ));
    }

    #[test]
    fn only_the_canonical_rekey_action_set_is_a_rekey() {
        use crate::squads::{ConfigAction, Member, Permissions};

        let members = vec![
            Member {
                key: Pubkey::new_unique(),
                permissions: Permissions::all(),
            },
            Member {
                key: Pubkey::new_unique(),
                permissions: Permissions::all(),
            },
        ];

        // The genuine brick set built for these members.
        let canonical = build_config_actions_for_kind(TransactionKind::Rekey, &members).unwrap();
        assert!(is_canonical_rekey(&canonical, &members));

        // A member-adding config change is NOT a rekey.
        let add_attacker = vec![ConfigAction::AddMember {
            new_member: Member {
                key: Pubkey::new_unique(),
                permissions: Permissions::all(),
            },
        }];
        assert!(!is_canonical_rekey(&add_attacker, &members));

        // A lone threshold drop is NOT a rekey.
        let lower_threshold = vec![ConfigAction::ChangeThreshold { new_threshold: 1 }];
        assert!(!is_canonical_rekey(&lower_threshold, &members));

        // The canonical set with an extra malicious action appended is NOT a rekey.
        let mut tampered = canonical.clone();
        tampered.push(ConfigAction::AddMember {
            new_member: Member {
                key: Pubkey::new_unique(),
                permissions: Permissions::all(),
            },
        });
        assert!(!is_canonical_rekey(&tampered, &members));
    }

    #[test]
    fn squads_instructions_classify_as_child_action() {
        let (multisig, _feature_gate) = setup();
        let child = compile(&[Instruction {
            program_id: SQUADS_MULTISIG_PROGRAM_ID,
            accounts: vec![],
            data: vec![1, 2, 3],
        }]);
        assert!(matches!(
            classify_vault_message(&child, &multisig),
            ProposalKind::ChildAction
        ));
    }
}
