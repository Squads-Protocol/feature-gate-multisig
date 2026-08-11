use eyre::Result;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

use crate::squads::{
    deserialize_squads_account, get_vault_pda, Multisig as SquadsMultisig, TransactionMessage,
    MULTISIG_ACCOUNT_DISCRIMINATOR, PERMISSION_EXECUTE, PERMISSION_INITIATE, PERMISSION_VOTE,
    PROPOSAL_APPROVE_DISCRIMINATOR, PROPOSAL_REJECT_DISCRIMINATOR, SQUADS_MULTISIG_PROGRAM_ID,
    VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
};
use crate::{
    output,
    provision::{
        create_child_create_config_transaction_and_proposal_message,
        create_child_execute_config_transaction_message, create_child_execute_transaction_message,
        create_config_transaction_create_instruction, create_execute_config_transaction_message,
        create_execute_transaction_message, create_proposal_create_instruction, create_rpc_client,
        create_transaction_and_proposal_message, create_vote_proposal_message,
        fetch_squads_multisig, get_proposal_status_and_threshold,
    },
    utils::{
        choose_network_from_config, create_child_vote_approve_transaction_message,
        create_child_vote_reject_transaction_message, load_signer, Config,
    },
};
use inquire::Confirm;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransactionKind {
    Activate,
    Revoke,
    Rekey,
}

#[derive(Debug, Clone, Copy)]
pub enum ProposalAction {
    Approve,
    Reject,
    Execute,
    Create,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildTransactionFlavor {
    Vault,
    Config,
}

#[derive(Debug, Clone, Copy)]
enum ProposalFlavor {
    Vault(TransactionKind),
    Config,
}

#[derive(Clone)]
enum ParentFlowPayload {
    None,
    Execute(Vec<solana_instruction::AccountMeta>),
    Create(TransactionMessage),
}

impl TransactionKind {
    pub fn label(&self) -> &'static str {
        match self {
            TransactionKind::Activate => "Feature Gate Activate",
            TransactionKind::Revoke => "Feature Gate Revoke",
            TransactionKind::Rekey => "Rekey Multisig",
        }
    }
}

/// Build config actions for multisig configuration operations.
/// Returns a vector of ConfigActions appropriate for the transaction kind.
pub(crate) fn build_config_actions_for_kind(
    kind: TransactionKind,
    multisig_members: &[crate::squads::Member],
) -> Result<Vec<crate::squads::ConfigAction>> {
    match kind {
        TransactionKind::Rekey => {
            let mut actions = Vec::new();

            // Add dummy PDA member first (Pubkey::default() cannot sign)
            // This ensures we always have at least 1 member before changing threshold
            // Add a dummy member with full permissions. This satisfies multisig invariants
            // (requires at least one proposer/voter/executor) while remaining practically
            // unusable because the dummy Pubkey::default() cannot sign.
            actions.push(crate::squads::ConfigAction::AddMember {
                new_member: crate::squads::Member {
                    key: Pubkey::default(),
                    permissions: crate::squads::Permissions::all(),
                },
            });

            // Set threshold to 1 (only the dummy member will remain)
            // Safe because we just added the dummy member
            actions.push(crate::squads::ConfigAction::ChangeThreshold { new_threshold: 1 });

            // Remove all current members last (after adding dummy and setting threshold)
            for member in multisig_members {
                actions.push(crate::squads::ConfigAction::RemoveMember {
                    old_member: member.key,
                });
            }

            Ok(actions)
        }
        // Other transaction kinds (Activate, Revoke) use vault transactions, not config transactions
        TransactionKind::Activate | TransactionKind::Revoke => Err(eyre::eyre!(
            "build_config_actions_for_kind called with non-config TransactionKind"
        )),
    }
}

/// Attempt to load an account as a Squads multisig. Returns Ok(None) if the account
/// doesn't exist, isn't owned by the Squads program, or doesn't carry the Multisig
/// account discriminator - this gates the EOA-vs-parent-multisig flow, so spoofable
/// lookalike data must not pass.
fn load_multisig_if_any(
    rpc_client: &solana_rpc_client::rpc_client::RpcClient,
    key: &Pubkey,
) -> Result<Option<SquadsMultisig>> {
    let acc = match rpc_client.get_account(key) {
        Ok(acc) => acc,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("AccountNotFound") || msg.contains("could not find account") {
                return Ok(None);
            }
            return Err(eyre::eyre!("Failed to fetch account {}: {}", key, e));
        }
    };

    if acc.owner != SQUADS_MULTISIG_PROGRAM_ID {
        return Ok(None);
    }

    Ok(deserialize_squads_account(&acc.data, MULTISIG_ACCOUNT_DISCRIMINATOR, "multisig").ok())
}

/// Check if a key is a member of the multisig with the required permission mask.
/// Returns (is_member, has_permission).
fn check_member_permission(ms: &SquadsMultisig, key: &Pubkey, permission_mask: u8) -> (bool, bool) {
    let member = ms.members.iter().find(|m| m.key == *key);
    let is_member = member.is_some();
    let has_permission = member
        .map(|m| (m.permissions.mask & permission_mask) == permission_mask)
        .unwrap_or(false);
    (is_member, has_permission)
}

/// Verify a key has the required permission, returning descriptive error if not.
fn verify_member_permission(
    ms: &SquadsMultisig,
    key: &Pubkey,
    permission: u8,
    role_name: &str,
) -> Result<()> {
    let (is_member, has_permission) = check_member_permission(ms, key, permission);
    if !is_member {
        output::Output::error(&format!(
            "{} key is not a member of the multisig.",
            role_name
        ));
        return Err(eyre::eyre!("{} must be a multisig member", role_name));
    }
    if !has_permission {
        let perm_name = match permission {
            PERMISSION_VOTE => "Vote",
            PERMISSION_EXECUTE => "Execute",
            PERMISSION_INITIATE => "Initiate",
            _ => "required",
        };
        output::Output::error(&format!(
            "{} key does not have {} permission.",
            role_name, perm_name
        ));
        return Err(eyre::eyre!(
            "Missing {} permission for {}",
            perm_name,
            role_name.to_lowercase()
        ));
    }
    Ok(())
}

/// In EOA (single-signer) mode the voting key is itself a required signer, but only the
/// fee payer keypair is supplied to sign. Fail fast with a clear message when they differ
/// instead of letting the transaction fail on-chain for a missing signature.
fn require_eoa_voting_key_matches_fee_payer(voting_key: &Pubkey, fee_payer: &Pubkey) -> Result<()> {
    if voting_key != fee_payer {
        return Err(eyre::eyre!(
            "In EOA mode, voting_key must match fee_payer. Got voting_key={voting_key}, fee_payer={fee_payer}"
        ));
    }
    Ok(())
}

/// Interactive confirmation - auto-confirms in E2E test mode. With `--yes`,
/// resolves to the prompt's default answer, so safety prompts that default to
/// "no" still abort rather than being force-approved.
pub(crate) fn confirm_action(prompt: &str, default: bool) -> bool {
    if crate::utils::is_e2e_test_mode() {
        return true;
    }
    if crate::utils::is_assume_yes() {
        return default;
    }
    Confirm::new(prompt)
        .with_default(default)
        .prompt()
        .unwrap_or(false)
}

/// Warn when the newest proposal is already the one about to be created. A send
/// whose confirmation timed out may still have landed, and a rerun allocates the
/// next index rather than noticing.
fn warn_on_duplicate_proposal(
    rpc_client: &solana_rpc_client::rpc_client::RpcClient,
    multisig: &Pubkey,
    latest_index: u64,
    kind: TransactionKind,
) {
    if latest_index == 0 {
        return;
    }
    let existing =
        crate::commands::proposal::describe_transaction(rpc_client, multisig, latest_index);
    if existing.transaction_kind() != Some(kind) {
        return;
    }
    // Only a proposal that can still be acted on is a duplicate worth flagging.
    let still_open = matches!(
        get_proposal_status_and_threshold(multisig, latest_index, rpc_client),
        Ok((_, _, crate::squads::ProposalStatus::Draft { .. }))
            | Ok((_, _, crate::squads::ProposalStatus::Active { .. }))
            | Ok((_, _, crate::squads::ProposalStatus::Approved { .. }))
    );
    if still_open {
        output::Output::warning(&format!(
            "Proposal #{latest_index} is already an unexecuted {}. If a previous run timed out it may have landed; creating another leaves two.",
            kind.label()
        ));
    }
}

/// Disclose what a pending config change does to the multisig, then take an
/// explicit decision on it. Returns false if the operator declines.
///
/// A canonical rekey is irreversible: it adds an unsignable member, drops the
/// threshold to 1, and removes everyone who can actually sign. Until now that
/// was disclosed only when *creating* one - the point where it is approved and
/// executed showed a bare label. So this states the resulting threshold and the
/// count of members who could still cast a vote, rather than leaving a signer to
/// derive them from the action list, and defaults to no. Defaulting to no also
/// means `--yes` aborts here instead of resolving it to a silent yes.
/// Authenticated read of the transaction at `index`. Returns None when the
/// account is a vault transaction rather than a config one, so callers select
/// the disclosure from the account discriminator instead of a caller-supplied
/// `--kind`. One read both decides and discloses, so a substituting endpoint
/// cannot answer the classification honestly and the disclosure differently.
fn read_config_transaction(
    rpc_client: &solana_rpc_client::rpc_client::RpcClient,
    multisig_address: &Pubkey,
    transaction_index: u64,
) -> Result<Option<crate::squads::ConfigTransaction>> {
    use crate::squads::{
        get_transaction_pda, ConfigTransaction, CONFIG_TRANSACTION_ACCOUNT_DISCRIMINATOR,
    };

    let (transaction_pda, _) = get_transaction_pda(multisig_address, transaction_index);
    let data = crate::provision::get_squads_account_data_with_retry(rpc_client, &transaction_pda)?;
    let Ok(config_tx) = deserialize_squads_account::<ConfigTransaction>(
        &data,
        CONFIG_TRANSACTION_ACCOUNT_DISCRIMINATOR,
        "config transaction",
    ) else {
        return Ok(None);
    };
    if config_tx.multisig != *multisig_address || config_tx.index != transaction_index {
        return Err(eyre::eyre!(
            "Config transaction at index {transaction_index} records multisig {} index {}. Refusing to disclose or act on it.",
            config_tx.multisig,
            config_tx.index
        ));
    }
    Ok(Some(config_tx))
}

fn confirm_config_change(
    rpc_client: &solana_rpc_client::rpc_client::RpcClient,
    multisig_address: &Pubkey,
    transaction_index: u64,
    config_tx: &crate::squads::ConfigTransaction,
    verb: &str,
) -> Result<bool> {
    use crate::squads::ConfigAction;

    let multisig = fetch_squads_multisig(rpc_client, multisig_address, "multisig")?;

    output::Output::header(&format!(
        "Config change at index {transaction_index} on multisig {multisig_address}"
    ));

    // Replay the actions over the current members to state the end result
    // instead of only the steps that get there.
    let mut members = multisig.members.clone();
    let mut threshold = multisig.threshold;
    for (i, action) in config_tx.actions.iter().enumerate() {
        match action {
            ConfigAction::AddMember { new_member } => {
                let unsignable = if new_member.key == Pubkey::default() {
                    " (unsignable placeholder key)"
                } else {
                    ""
                };
                output::Output::numbered_field(
                    i + 1,
                    "Add member",
                    &format!("{}{}", new_member.key, unsignable),
                );
                members.push(new_member.clone());
            }
            ConfigAction::RemoveMember { old_member } => {
                output::Output::numbered_field(i + 1, "Remove member", &old_member.to_string());
                members.retain(|m| m.key != *old_member);
            }
            ConfigAction::ChangeThreshold { new_threshold } => {
                output::Output::numbered_field(i + 1, "Set threshold", &new_threshold.to_string());
                threshold = *new_threshold;
            }
        }
    }

    // A member whose key is the default pubkey holds no private key, so it can
    // never sign - it counts towards Squads' invariants but not towards anyone
    // being able to govern the multisig afterwards.
    let usable_voters = members
        .iter()
        .filter(|m| m.key != Pubkey::default() && (m.permissions.mask & PERMISSION_VOTE) != 0)
        .count();

    output::Output::field("Resulting threshold", &threshold.to_string());
    output::Output::field(
        "Members able to vote afterwards",
        &usable_voters.to_string(),
    );

    if usable_voters == 0 {
        output::Output::warning(
            "No remaining member can sign. This permanently disables voting on this multisig, and it cannot be undone.",
        );
    } else if usable_voters < usize::from(threshold) {
        output::Output::warning(&format!(
            "Only {usable_voters} member(s) can sign but the threshold is {threshold}; the multisig will be unable to reach quorum."
        ));
    }

    let approved = confirm_action(&format!("{verb} this config change?"), false);
    // Declining interactively is an ordinary "not now". Declining because `--yes`
    // resolved this to its `false` default is different: the caller asked not to
    // be prompted and the tool refused to answer on their behalf, so it must not
    // exit 0 as though the config change had been handled.
    if !approved && crate::utils::is_assume_yes() {
        return Err(eyre::eyre!(
            "Refusing to {} a config change under --yes: it permanently changes who governs this multisig. Re-run without --yes to review the actions above and decide.",
            verb.to_lowercase()
        ));
    }
    Ok(approved)
}

/// Pre-flight check before acting on a feature gate: surface any state warnings
/// for `kind` (e.g. revoke on a non-pending feature) and confirm. Returns false
/// if the user declines.
fn confirm_feature_preflight(
    rpc_client: &solana_rpc_client::rpc_client::RpcClient,
    multisig: &Pubkey,
    kind: TransactionKind,
) -> Result<bool> {
    let verification = crate::verification::verify_feature_gate(rpc_client, multisig)?;
    let warnings = crate::verification::feature_action_warnings(&verification, kind);
    if warnings.is_empty() {
        return Ok(true);
    }
    output::Output::warning("Feature gate pre-flight raised concerns:");
    for warning in &warnings {
        output::Output::warning(warning);
    }
    Ok(confirm_action("Proceed despite the above?", false))
}

/// Permission mask, display name, and action description required on the child
/// multisig for a parent-flow operation.
fn operation_requirements(
    operation: ProposalAction,
    kind: TransactionKind,
    proposal_index: u64,
) -> (u8, &'static str, String) {
    match operation {
        ProposalAction::Approve => (
            PERMISSION_VOTE,
            "Vote",
            format!("Approve {} at index {}", kind.label(), proposal_index),
        ),
        ProposalAction::Reject => (
            PERMISSION_VOTE,
            "Vote",
            format!("Reject {} at index {}", kind.label(), proposal_index),
        ),
        ProposalAction::Execute => (
            PERMISSION_EXECUTE,
            "Execute",
            format!("Execute {} at index {}", kind.label(), proposal_index),
        ),
        ProposalAction::Create => (
            PERMISSION_INITIATE | PERMISSION_VOTE,
            "Initiate+Vote",
            format!(
                "Create {} proposal at index {}",
                kind.label(),
                proposal_index
            ),
        ),
    }
}

/// The fee payer creates the parent transaction and proposal, so it must be a
/// parent member with Initiate permission.
fn ensure_fee_payer_can_initiate(
    parent_ms: &SquadsMultisig,
    fee_payer: &Pubkey,
    parent_multisig: &Pubkey,
) -> Result<()> {
    let (is_member, has_initiate) =
        check_member_permission(parent_ms, fee_payer, PERMISSION_INITIATE);
    if !is_member {
        output::Output::error(
            "Fee payer must be a member of the parent multisig to create/approve/execute proposals.",
        );
        output::Output::hint(&format!(
            "Current fee payer: {}\nParent multisig: {}\nAdd the fee payer as a member (with Initiate permission) or choose a fee payer that is already a member with Initiate.",
            fee_payer, parent_multisig,
        ));
        return Err(eyre::eyre!(
            "Fee payer is not a member of the parent multisig"
        ));
    }
    if !has_initiate {
        output::Output::error("Fee payer lacks Initiate permission on the parent multisig.");
        return Err(eyre::eyre!(
            "Fee payer missing Initiate permission on parent multisig"
        ));
    }
    Ok(())
}

/// The parent multisig acts on the child by signing via CPI as its vault PDA, so
/// the child must list that PDA (not the parent multisig address) as a member
/// with the operation's required permission.
fn ensure_parent_vault_member_on_child(
    child_ms: &SquadsMultisig,
    parent_vault_member: &Pubkey,
    parent_multisig: &Pubkey,
    required_permission_mask: u8,
    permission_name: &str,
) -> Result<()> {
    let (_, parent_vault_has_permission) =
        check_member_permission(child_ms, parent_vault_member, required_permission_mask);
    if parent_vault_has_permission {
        return Ok(());
    }

    let parent_multisig_is_member = child_ms.members.iter().any(|m| m.key == *parent_multisig);
    if parent_multisig_is_member {
        output::Output::error("Child multisig has the parent multisig address as a member, but not the parent vault PDA. The parent program cannot sign as the multisig address during CPI.");
        output::Output::hint(&format!(
            "Please add the parent vault PDA as a member with {} permission on the child: {}",
            permission_name, parent_vault_member
        ));
    } else {
        output::Output::error(&format!(
            "Child multisig is missing the parent vault PDA as a member with {} permission.",
            permission_name
        ));
        output::Output::hint(&format!(
            "Add this key as a member with {}: {}",
            permission_name, parent_vault_member
        ));
    }
    Err(eyre::eyre!(
        "Child multisig missing required member for programmatic {}",
        permission_name.to_lowercase()
    ))
}

/// Build the child-multisig instruction message that the parent proposal wraps,
/// dispatching on the operation, the child transaction flavor, and the payload.
fn build_child_transaction_message(
    operation: ProposalAction,
    child_flavor: ChildTransactionFlavor,
    payload: ParentFlowPayload,
    child_multisig: Pubkey,
    proposal_index: u64,
    parent_vault_member: Pubkey,
    fee_payer: Pubkey,
) -> Result<TransactionMessage> {
    match (operation, child_flavor, payload) {
        (ProposalAction::Approve, _, ParentFlowPayload::None) => {
            create_child_vote_approve_transaction_message(
                child_multisig,
                proposal_index,
                parent_vault_member,
            )
        }
        (ProposalAction::Reject, _, ParentFlowPayload::None) => {
            create_child_vote_reject_transaction_message(
                child_multisig,
                proposal_index,
                parent_vault_member,
            )
        }
        (
            ProposalAction::Execute,
            ChildTransactionFlavor::Vault,
            ParentFlowPayload::Execute(exec),
        ) => create_child_execute_transaction_message(
            child_multisig,
            proposal_index,
            parent_vault_member,
            exec,
        ),
        (ProposalAction::Execute, ChildTransactionFlavor::Config, ParentFlowPayload::None) => {
            create_child_execute_config_transaction_message(
                child_multisig,
                proposal_index,
                parent_vault_member,
            )
        }
        (ProposalAction::Create, ChildTransactionFlavor::Vault, ParentFlowPayload::Create(msg)) => {
            crate::provision::create_child_create_vault_transaction_and_proposal_message(
                child_multisig,
                proposal_index,
                parent_vault_member,
                fee_payer,
                msg,
            )
        }
        (ProposalAction::Create, _, ParentFlowPayload::Create(msg)) => Ok(msg),
        (ProposalAction::Execute, ChildTransactionFlavor::Vault, _) => Err(eyre::eyre!(
            "Execution accounts required for Execute operation"
        )),
        (ProposalAction::Create, _, _) => Err(eyre::eyre!(
            "Creation message required for Create operation"
        )),
        _ => Err(eyre::eyre!(
            "Invalid parent flow payload for requested operation"
        )),
    }
}

/// Approve the just-created parent proposal and, when approvals already meet the
/// threshold and the fee payer may execute, offer to execute it immediately.
fn approve_and_maybe_execute_parent(
    rpc_client: &solana_rpc_client::rpc_client::RpcClient,
    parent_multisig: &Pubkey,
    parent_ms: &SquadsMultisig,
    parent_proposal_index: u64,
    fee_payer_signer: &dyn Signer,
    operation: ProposalAction,
) -> Result<()> {
    // Fetch a fresh blockhash: confirming the create transaction above can take
    // long enough for the previous one to expire.
    let approve_blockhash = rpc_client.get_latest_blockhash()?;
    let approve_msg = create_vote_proposal_message(
        parent_multisig,
        &fee_payer_signer.pubkey(),
        &fee_payer_signer.pubkey(),
        parent_proposal_index,
        approve_blockhash,
        PROPOSAL_APPROVE_DISCRIMINATOR,
    )?;
    let approve_tx =
        VersionedTransaction::try_new(VersionedMessage::V0(approve_msg), &[fee_payer_signer])?;
    let approve_sig = crate::provision::send_and_confirm_transaction(&approve_tx, rpc_client)
        .map_err(|e| eyre::eyre!("Failed to approve parent proposal: {}", e))?;
    output::Output::field("Parent proposal approved (sig)", &approve_sig);

    // If approvals meet threshold, offer to execute the parent proposal now
    let (approved, threshold, _status) =
        get_proposal_status_and_threshold(parent_multisig, parent_proposal_index, rpc_client)?;
    let (is_fee_payer_member, has_execute_permission) =
        check_member_permission(parent_ms, &fee_payer_signer.pubkey(), PERMISSION_EXECUTE);

    if approved as u16 >= threshold
        && is_fee_payer_member
        && has_execute_permission
        && confirm_action("Execute this parent proposal now?", true)
    {
        let fresh_blockhash = rpc_client.get_latest_blockhash()?;
        // Parent multisig always stores vault transactions, even when creating config on child
        let exec_msg = create_execute_transaction_message(
            parent_multisig,
            &fee_payer_signer.pubkey(),
            &fee_payer_signer.pubkey(),
            parent_proposal_index,
            rpc_client,
            fresh_blockhash,
        )?;
        let exec_tx =
            VersionedTransaction::try_new(VersionedMessage::V0(exec_msg), &[fee_payer_signer])?;
        let exec_sig = crate::provision::send_and_confirm_transaction(&exec_tx, rpc_client)
            .map_err(|e| eyre::eyre!("Failed to execute parent proposal: {}", e))?;
        output::Output::field("Parent proposal executed (sig)", &exec_sig);

        let success_message = match operation {
            ProposalAction::Approve => "Child proposal has been approved via parent multisig!",
            ProposalAction::Reject => "Child proposal has been rejected via parent multisig!",
            ProposalAction::Execute => "Child transaction has been executed via parent multisig!",
            ProposalAction::Create => "Child proposal has been created via parent multisig!",
        };
        output::Output::success(success_message);
    } else if !is_fee_payer_member {
        output::Output::hint(
            "Fee payer is not a member of the parent multisig; cannot auto-execute.",
        );
    } else if !has_execute_permission {
        output::Output::hint("Executor does not have Execute permission on the parent multisig.");
    } else {
        output::Output::hint(&format!(
            "Parent approvals {}/{} - waiting for more confirmations before execution.",
            approved, threshold
        ));
    }
    Ok(())
}

/// Common flow for creating and optionally executing a parent multisig proposal
/// that operates on a child multisig.
///
/// This handles:
/// - Validating parent vault PDA has required permissions on child
/// - Creating parent transaction+proposal
/// - Optionally approving the parent proposal
/// - Optionally executing the parent proposal (if threshold met)
// Orchestrates one parent-multisig action (create/approve/reject/execute); the parameters are
// independent context (ids, signer, network, operation, payload) that don't form a cohesive group.
#[allow(clippy::too_many_arguments)]
fn handle_parent_multisig_flow(
    parent_multisig: Pubkey,
    feature_gate_multisig_address: Pubkey,
    proposal_index: u64,
    kind: TransactionKind,
    child_flavor: ChildTransactionFlavor,
    fee_payer_signer: &dyn Signer,
    rpc_url: &str,
    operation: ProposalAction,
    payload: ParentFlowPayload,
) -> Result<()> {
    let rpc_client = create_rpc_client(rpc_url);

    // Fetch parent's next transaction index
    let parent_ms = fetch_squads_multisig(&rpc_client, &parent_multisig, "parent multisig")?;
    let parent_next_index = parent_ms.transaction_index + 1;

    ensure_fee_payer_can_initiate(&parent_ms, &fee_payer_signer.pubkey(), &parent_multisig)?;

    // The parent acts on the child as its vault PDA, which must be a child member.
    let parent_vault_member = get_vault_pda(&parent_multisig, 0).0;
    let child_ms = fetch_squads_multisig(
        &rpc_client,
        &feature_gate_multisig_address,
        "child multisig",
    )?;
    let (required_permission_mask, permission_name, action_description) =
        operation_requirements(operation, kind, proposal_index);
    ensure_parent_vault_member_on_child(
        &child_ms,
        &parent_vault_member,
        &parent_multisig,
        required_permission_mask,
        permission_name,
    )?;

    // Display information about the parent multisig transaction
    output::Output::header(&format!(
        "Parent Multisig {} Transaction Details:",
        permission_name
    ));
    output::Output::field("Parent Multisig", &parent_multisig.to_string());
    output::Output::field("Child Multisig", &feature_gate_multisig_address.to_string());
    output::Output::field("Parent Transaction Index", &parent_next_index.to_string());
    output::Output::field("Child Proposal Index", &proposal_index.to_string());
    // Describe the child by what it is on-chain, not by the kind the caller
    // named. For an existing proposal those can differ - the caller's kind is
    // unverified input - and this line is what parent members read before
    // voting. Creation is the one case with nothing yet to classify, so there
    // the requested kind is the truth.
    let on_chain_kind = match operation {
        ProposalAction::Create => None,
        _ => Some(crate::commands::proposal::describe_transaction(
            &rpc_client,
            &feature_gate_multisig_address,
            proposal_index,
        )),
    };
    let type_label = match on_chain_kind {
        Some(on_chain) => on_chain.label().to_string(),
        None => kind.label().to_string(),
    };
    output::Output::field("Transaction Type", &type_label);
    output::Output::field("Action", &action_description);

    // Ask for confirmation before creating the parent multisig proposal
    let confirmation_prompt = match operation {
        ProposalAction::Approve => "Create parent multisig proposal for this transaction?",
        ProposalAction::Reject => "Create parent multisig proposal to reject this transaction?",
        ProposalAction::Execute => "Create parent multisig proposal to execute child transaction?",
        ProposalAction::Create => {
            "Create parent multisig proposal to create a new child transaction/proposal?"
        }
    };
    // Authorizing a config change on the child is the irreversible class of
    // action, so the parent's vote on it is an explicit decision rather than a
    // default yes. Rejecting one is the safe direction and stays easy - a
    // default of no there would just make it harder to stop a rekey.
    //
    // Keyed off the on-chain class, not the caller's `--kind`: naming a config
    // transaction `--kind activate` previously restored the default yes.
    let child_is_config = matches!(child_flavor, ChildTransactionFlavor::Config)
        || on_chain_kind.is_some_and(|kind| kind.is_config_transaction());
    let default_answer = !matches!(
        (child_is_config, operation),
        (true, ProposalAction::Approve | ProposalAction::Execute)
    );
    if !confirm_action(confirmation_prompt, default_answer) {
        output::Output::info("Parent multisig proposal creation cancelled.");
        return Ok(());
    }

    let tx_message = build_child_transaction_message(
        operation,
        child_flavor,
        payload,
        feature_gate_multisig_address,
        proposal_index,
        parent_vault_member,
        fee_payer_signer.pubkey(),
    )?;

    if matches!(operation, ProposalAction::Create) {
        output::Output::warning(&format!(
            "The child transaction embeds the fee payer ({}) as a required signer (rent payer). \
             The parent proposal must be executed with this same fee payer.",
            fee_payer_signer.pubkey()
        ));
    }

    let blockhash = rpc_client.get_latest_blockhash()?;
    let (msg, tx_pda, prop_pda) = create_transaction_and_proposal_message(
        &fee_payer_signer.pubkey(),
        &fee_payer_signer.pubkey(),
        &parent_multisig,
        parent_next_index,
        0,
        tx_message,
        Some(crate::constants::DEFAULT_PRIORITY_FEE as u32),
        Some(crate::constants::DEFAULT_COMPUTE_UNITS),
        blockhash,
    )?;
    output::Output::address("Parent transaction PDA", &tx_pda.to_string());
    output::Output::address("Parent proposal PDA", &prop_pda.to_string());

    let transaction =
        VersionedTransaction::try_new(VersionedMessage::V0(msg), &[fee_payer_signer])?;
    let signature = crate::provision::send_and_confirm_transaction(&transaction, &rpc_client)
        .map_err(|e| eyre::eyre!("Failed to send parent transaction: {}", e))?;

    output::Output::header(&format!(
        "Parent Multisig {} Transaction Created & Sent:",
        permission_name
    ));
    output::Output::field("Signature", &signature);

    // Offer to approve the parent proposal immediately
    if confirm_action("Approve this parent proposal now?", true) {
        approve_and_maybe_execute_parent(
            &rpc_client,
            &parent_multisig,
            &parent_ms,
            parent_next_index,
            fee_payer_signer,
            operation,
        )?;
    }
    Ok(())
}

pub fn approve_common_feature_gate_proposal(
    config: &Config,
    feature_gate_multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    proposal_index: u64,
    kind: TransactionKind,
) -> Result<()> {
    approve_common_proposal(
        config,
        feature_gate_multisig_address,
        voting_key,
        fee_payer_path,
        proposal_index,
        ProposalFlavor::Vault(kind),
    )
}

pub fn reject_common_feature_gate_proposal(
    config: &Config,
    feature_gate_multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    proposal_index: u64,
    kind: TransactionKind,
) -> Result<()> {
    let fee_payer_signer = load_signer(&fee_payer_path, "fee payer")?;

    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = create_rpc_client(&rpc_url);

    let is_parent_multisig = load_multisig_if_any(&rpc_client, &voting_key)?.is_some();

    if is_parent_multisig {
        let child_flavor = if kind == TransactionKind::Rekey {
            ChildTransactionFlavor::Config
        } else {
            ChildTransactionFlavor::Vault
        };

        return handle_parent_multisig_flow(
            voting_key,
            feature_gate_multisig_address,
            proposal_index,
            kind,
            child_flavor,
            &fee_payer_signer,
            &rpc_url,
            ProposalAction::Reject,
            ParentFlowPayload::None,
        );
    }

    // EOA voting path: voting_key must be a member with Vote permission (preflight,
    // mirroring approve), and must equal the fee payer since only the fee payer signs.
    let child_ms = fetch_squads_multisig(&rpc_client, &feature_gate_multisig_address, "multisig")?;
    verify_member_permission(&child_ms, &voting_key, PERMISSION_VOTE, "Rejecter")?;
    require_eoa_voting_key_matches_fee_payer(&voting_key, &fee_payer_signer.pubkey())?;

    let blockhash = rpc_client.get_latest_blockhash()?;
    let transaction_message = create_vote_proposal_message(
        &feature_gate_multisig_address,
        &voting_key,
        &fee_payer_signer.pubkey(),
        proposal_index,
        blockhash,
        PROPOSAL_REJECT_DISCRIMINATOR,
    )
    .map_err(|e| {
        eyre::eyre!(
            "Failed to create reject {} transaction message: {}",
            kind.label(),
            e
        )
    })?;

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(transaction_message),
        &[fee_payer_signer.as_ref()],
    )?;

    if !confirm_action("Send this reject transaction now?", true) {
        output::Output::hint("Skipped sending. You can submit the above encoded transaction manually or rerun to send.");
        return Ok(());
    }

    let signature = crate::provision::send_and_confirm_transaction(&transaction, &rpc_client)
        .map_err(|e| eyre::eyre!("Failed to send reject transaction: {}", e))?;
    output::Output::field("Reject transaction sent successfully", &signature);
    Ok(())
}

pub fn execute_common_feature_gate_proposal(
    config: &Config,
    feature_gate_multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    proposal_index: u64,
    kind: TransactionKind,
) -> Result<()> {
    let fee_payer_signer = load_signer(&fee_payer_path, "fee payer")?;

    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = create_rpc_client(&rpc_url);

    // Pre-flight: warn if the feature state is wrong for this action
    // (e.g. revoking something that is not pending fails on-chain).
    if !confirm_feature_preflight(&rpc_client, &feature_gate_multisig_address, kind)? {
        output::Output::info("Cancelled.");
        return Ok(());
    }

    // Readiness check: ensure approvals >= threshold on child proposal
    let (approved, threshold, _status) = get_proposal_status_and_threshold(
        &feature_gate_multisig_address,
        proposal_index,
        &rpc_client,
    )?;
    if (approved as u16) < threshold {
        output::Output::hint(&format!(
            "Proposal at index {} not ready to execute yet: approvals {}/{}",
            proposal_index, approved, threshold
        ));
        return Err(eyre::eyre!("Insufficient approvals to execute"));
    }

    let is_parent_multisig = load_multisig_if_any(&rpc_client, &voting_key)?.is_some();

    if is_parent_multisig {
        // If this is a config transaction (Rekey), use the config execute path without extra metas.
        if kind == TransactionKind::Rekey {
            return handle_parent_multisig_flow(
                voting_key,
                feature_gate_multisig_address,
                proposal_index,
                TransactionKind::Rekey,
                ChildTransactionFlavor::Config,
                &fee_payer_signer,
                &rpc_url,
                ProposalAction::Execute,
                ParentFlowPayload::None,
            );
        }

        // Vault transaction execute path
        use crate::squads::{get_transaction_pda, VaultTransaction};
        let (child_transaction_pda, _) =
            get_transaction_pda(&feature_gate_multisig_address, proposal_index);
        let child_transaction_account_data = rpc_client
            .get_account_data(&child_transaction_pda)
            .map_err(|e| eyre::eyre!("Failed to fetch child transaction account: {}", e))?;
        let child_transaction_contents: VaultTransaction = deserialize_squads_account(
            &child_transaction_account_data,
            VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
            "child vault transaction",
        )?;
        let child_transaction_message = child_transaction_contents.message;

        // Build execution account metas from the child transaction.
        let child_execution_account_metas = child_transaction_message.execution_account_metas()?;

        return handle_parent_multisig_flow(
            voting_key,
            feature_gate_multisig_address,
            proposal_index,
            kind,
            ChildTransactionFlavor::Vault,
            &fee_payer_signer,
            &rpc_url,
            ProposalAction::Execute,
            ParentFlowPayload::Execute(child_execution_account_metas),
        );
    }

    // Permission check: voting_key must be a member with Execute permission on the child multisig
    let child_ms = fetch_squads_multisig(&rpc_client, &feature_gate_multisig_address, "multisig")?;

    verify_member_permission(&child_ms, &voting_key, PERMISSION_EXECUTE, "Executor")?;
    require_eoa_voting_key_matches_fee_payer(&voting_key, &fee_payer_signer.pubkey())?;

    // Fresh blockhash and build execute message for the proposal
    let blockhash = rpc_client.get_latest_blockhash()?;
    let exec_msg = create_execute_transaction_message(
        &feature_gate_multisig_address,
        &voting_key,
        &fee_payer_signer.pubkey(),
        proposal_index,
        &rpc_client,
        blockhash,
    )?;

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(exec_msg),
        &[fee_payer_signer.as_ref()],
    )?;

    // Confirm before sending on-chain (EOA execute path)
    if !confirm_action("Send this execute transaction now?", true) {
        output::Output::hint("Skipped sending. You can submit the above encoded transaction manually or rerun to send.");
        return Ok(());
    }

    // Send and confirm
    let signature = crate::provision::send_and_confirm_transaction(&transaction, &rpc_client)
        .map_err(|e| eyre::eyre!("Failed to execute proposal: {}", e))?;
    output::Output::field("Proposal executed (sig)", &signature);
    Ok(())
}

pub fn create_feature_gate_proposal(
    config: &Config,
    feature_gate_multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    kind: TransactionKind,
) -> Result<()> {
    let fee_payer_signer = load_signer(&fee_payer_path, "fee payer")?;

    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = create_rpc_client(&rpc_url);

    // Fetch the feature gate multisig to get transaction index
    let feature_gate_ms = fetch_squads_multisig(
        &rpc_client,
        &feature_gate_multisig_address,
        "feature gate multisig",
    )?;

    let next_tx_index = feature_gate_ms.transaction_index + 1;
    warn_on_duplicate_proposal(
        &rpc_client,
        &feature_gate_multisig_address,
        feature_gate_ms.transaction_index,
        kind,
    );

    // Feature ID is the child vault address (vault 0)
    let feature_id = get_vault_pda(&feature_gate_multisig_address, 0).0;

    // Pre-flight: warn if the feature state is wrong for this action.
    if !confirm_feature_preflight(&rpc_client, &feature_gate_multisig_address, kind)? {
        output::Output::info("Cancelled.");
        return Ok(());
    }

    // Detect if voting_key is itself a Squads multisig (parent)
    let is_parent_multisig = load_multisig_if_any(&rpc_client, &voting_key)?.is_some();

    output::Output::header(&format!("🎯 Create {} Proposal", kind.label()));
    output::Output::field("Feature ID", &feature_id.to_string());
    output::Output::field("Multisig", &feature_gate_multisig_address.to_string());
    output::Output::field("Next Vault Transaction Index", &next_tx_index.to_string());

    if !matches!(kind, TransactionKind::Activate | TransactionKind::Revoke) {
        return Err(eyre::eyre!("Unsupported kind for feature gate proposal"));
    }

    if is_parent_multisig {
        output::Output::info("Parent multisig detected - creating child vault proposal");

        // voting_key is the parent multisig address, derive the vault PDA from it
        let parent_vault_pda = crate::squads::get_vault_pda(&voting_key, 0).0;
        output::Output::field(
            "Parent vault PDA (creator/rent_payer)",
            &parent_vault_pda.to_string(),
        );

        let vault_tx_message =
            crate::provision::create_feature_gate_transaction_message(feature_id, kind)?;

        // Pass the raw vault transaction message - handle_parent_multisig_flow will wrap it
        // in create_child_create_vault_transaction_and_proposal_message
        handle_parent_multisig_flow(
            voting_key,
            feature_gate_multisig_address,
            next_tx_index,
            kind,
            ChildTransactionFlavor::Vault,
            &fee_payer_signer,
            &rpc_url,
            ProposalAction::Create,
            ParentFlowPayload::Create(vault_tx_message),
        )?;

        output::Output::field(
            "Vault proposal created at index",
            &next_tx_index.to_string(),
        );

        return Ok(());
    }

    // EOA voting path - create vault proposal.
    output::Output::info(&format!("Creating {} proposal...", kind.label()));

    // Verify voting_key has Initiate permission
    verify_member_permission(
        &feature_gate_ms,
        &voting_key,
        PERMISSION_INITIATE,
        "Creator",
    )?;

    require_eoa_voting_key_matches_fee_payer(&voting_key, &fee_payer_signer.pubkey())?;

    let vault_message =
        crate::provision::create_feature_gate_transaction_message(feature_id, kind)?;

    let blockhash = rpc_client.get_latest_blockhash()?;
    let (message, _tx_pda, _proposal_pda) = create_transaction_and_proposal_message(
        &fee_payer_signer.pubkey(),
        &voting_key,
        &feature_gate_multisig_address,
        next_tx_index,
        0,
        vault_message,
        Some(crate::constants::DEFAULT_PRIORITY_FEE as u32),
        Some(crate::constants::DEFAULT_COMPUTE_UNITS),
        blockhash,
    )?;
    let transaction =
        VersionedTransaction::try_new(VersionedMessage::V0(message), &[fee_payer_signer.as_ref()])?;

    // Every other send path confirms first; this one submitted straight away.
    if !confirm_action("Send this create transaction now?", true) {
        output::Output::hint("Skipped sending. Rerun to create the proposal.");
        return Ok(());
    }

    let signature = crate::provision::send_and_confirm_transaction(&transaction, &rpc_client)
        .map_err(|e| eyre::eyre!("Failed to create {} proposal: {}", kind.label(), e))?;

    output::Output::field("Vault proposal index", &next_tx_index.to_string());
    output::Output::field("Create proposal signature", &signature);

    output::Output::info(
        "Next step: Gather approvals from other members, then execute the proposal.",
    );
    Ok(())
}

pub fn rekey_multisig_feature_gate(
    config: &Config,
    feature_gate_multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
) -> Result<()> {
    let fee_payer_signer = load_signer(&fee_payer_path, "fee payer")?;

    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = create_rpc_client(&rpc_url);

    // Fetch the feature gate multisig to get its members
    let feature_gate_ms = fetch_squads_multisig(
        &rpc_client,
        &feature_gate_multisig_address,
        "feature gate multisig",
    )?;

    // Build config actions for rekey
    let actions = build_config_actions_for_kind(TransactionKind::Rekey, &feature_gate_ms.members)?;

    output::Output::header("🔑 Rekey Multisig - Config Actions");
    output::Output::field("Actions count", &actions.len().to_string());
    for (i, action) in actions.iter().enumerate() {
        match action {
            crate::squads::ConfigAction::RemoveMember { old_member } => {
                output::Output::numbered_field(i + 1, "Remove Member", &old_member.to_string());
            }
            crate::squads::ConfigAction::AddMember { new_member } => {
                output::Output::numbered_field(
                    i + 1,
                    "Add Member (Dummy)",
                    &new_member.key.to_string(),
                );
            }
            crate::squads::ConfigAction::ChangeThreshold { new_threshold } => {
                output::Output::numbered_field(i + 1, "Set Threshold", &new_threshold.to_string());
            }
        }
    }

    // Final confirmation. Routed through `confirm_action` so this prompt obeys
    // the same rules as every other one: auto-confirmed under E2E, and resolved
    // to its `false` default under `--yes` so automation cannot start a rekey
    // without a person deciding to.
    if !confirm_action(
        "This will permanently disable voting on this multisig. Are you sure?",
        false,
    ) {
        output::Output::hint("Rekey cancelled.");
        return Ok(());
    }

    // Fetch next transaction index (child)
    let next_tx_index = feature_gate_ms.transaction_index + 1;
    warn_on_duplicate_proposal(
        &rpc_client,
        &feature_gate_multisig_address,
        feature_gate_ms.transaction_index,
        TransactionKind::Rekey,
    );

    // Detect if voting_key is itself a Squads multisig (parent)
    let is_parent_multisig = load_multisig_if_any(&rpc_client, &voting_key)?.is_some();

    if is_parent_multisig {
        output::Output::info("Parent multisig voting detected - creating parent proposal on child");
        let parent_vault_member = get_vault_pda(&voting_key, 0).0;

        let child_tx_message = create_child_create_config_transaction_and_proposal_message(
            feature_gate_multisig_address,
            next_tx_index,
            parent_vault_member,
            fee_payer_signer.pubkey(),
            actions.clone(),
            Some("Rekey multisig - disable voting".to_string()),
        );

        return handle_parent_multisig_flow(
            voting_key,
            feature_gate_multisig_address,
            next_tx_index,
            TransactionKind::Rekey,
            ChildTransactionFlavor::Config,
            &fee_payer_signer,
            &rpc_url,
            ProposalAction::Create,
            ParentFlowPayload::Create(child_tx_message?),
        );
    }

    // EOA voting path - create config transaction and proposal
    output::Output::info("Creating config transaction for rekey...");

    // Get transaction & proposal PDAs
    let (tx_pda, _) =
        crate::squads::get_transaction_pda(&feature_gate_multisig_address, next_tx_index);
    let (proposal_pda, _) =
        crate::squads::get_proposal_pda(&feature_gate_multisig_address, next_tx_index);

    let create_tx_instruction = create_config_transaction_create_instruction(
        &feature_gate_multisig_address,
        &tx_pda,
        &voting_key,
        &fee_payer_signer.pubkey(),
        actions.clone(),
        Some("Rekey multisig - disable voting".to_string()),
    )?;
    let create_proposal_instruction = create_proposal_create_instruction(
        &feature_gate_multisig_address,
        &proposal_pda,
        &voting_key,
        &fee_payer_signer.pubkey(),
        next_tx_index,
    )?;

    // Confirm before sending. A raw prompt here resolved to `false` whenever no
    // terminal was attached, so this path silently returned success having
    // created nothing; `confirm_action` gives it the same E2E and `--yes`
    // handling as every other send prompt.
    if !confirm_action("Send rekey proposal now?", true) {
        output::Output::hint("Skipped sending. You can rerun to send.");
        return Ok(());
    }

    // Get fresh blockhash right before sending
    let blockhash = rpc_client.get_latest_blockhash()?;

    require_eoa_voting_key_matches_fee_payer(&voting_key, &fee_payer_signer.pubkey())?;

    // Build a v0 message for config rekey creation (tx + proposal)
    let v0_message = solana_message::v0::Message::try_compile(
        &fee_payer_signer.pubkey(),
        &[create_tx_instruction, create_proposal_instruction],
        &[],
        blockhash,
    )?;

    // In EOA mode, fee_payer IS the voting_key/creator - one signer covers both
    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(v0_message),
        &[fee_payer_signer.as_ref()],
    )?;

    // Send and confirm
    let signature = crate::provision::send_and_confirm_transaction(&transaction, &rpc_client)
        .map_err(|e| eyre::eyre!("Failed to create rekey proposal: {}", e))?;
    output::Output::field("Rekey proposal created (sig)", &signature);
    output::Output::field("Proposal index", &next_tx_index.to_string());

    // Offer to approve the config proposal immediately
    if Confirm::new("Approve this config proposal now?")
        .with_default(true)
        .prompt()
        .unwrap_or(false)
    {
        approve_common_config_change(
            config,
            feature_gate_multisig_address,
            voting_key,
            fee_payer_path,
            next_tx_index,
        )?;
    }

    output::Output::info(
        "Next step: Gather approvals from other members, then execute the config transaction.",
    );
    Ok(())
}

pub fn approve_common_config_change(
    config: &Config,
    multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    transaction_index: u64,
) -> Result<()> {
    approve_common_proposal(
        config,
        multisig_address,
        voting_key,
        fee_payer_path,
        transaction_index,
        ProposalFlavor::Config,
    )
}

fn approve_common_proposal(
    config: &Config,
    multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    proposal_index: u64,
    flavor: ProposalFlavor,
) -> Result<()> {
    let fee_payer_signer = load_signer(&fee_payer_path, "fee payer")?;

    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = create_rpc_client(&rpc_url);
    let blockhash = rpc_client.get_latest_blockhash()?;

    // Decide flavor/kind for parent flow and labeling
    let (child_flavor, kind_for_label) = match flavor {
        ProposalFlavor::Vault(kind) => (ChildTransactionFlavor::Vault, kind),
        ProposalFlavor::Config => (ChildTransactionFlavor::Config, TransactionKind::Rekey),
    };

    // Detect if voting_key is itself a Squads multisig (parent)
    let is_parent_multisig = load_multisig_if_any(&rpc_client, &voting_key)?.is_some();

    if is_parent_multisig {
        return handle_parent_multisig_flow(
            voting_key,
            multisig_address,
            proposal_index,
            kind_for_label,
            child_flavor,
            &fee_payer_signer,
            &rpc_url,
            ProposalAction::Approve,
            ParentFlowPayload::None,
        );
    }

    // EOA voting path - approve proposal
    // Validate voter membership and Vote permission on the child multisig
    let child_ms = fetch_squads_multisig(&rpc_client, &multisig_address, "multisig")?;

    verify_member_permission(&child_ms, &voting_key, PERMISSION_VOTE, "Approver")?;
    require_eoa_voting_key_matches_fee_payer(&voting_key, &fee_payer_signer.pubkey())?;

    let approve_msg = create_vote_proposal_message(
        &multisig_address,
        &voting_key,
        &fee_payer_signer.pubkey(),
        proposal_index,
        blockhash,
        PROPOSAL_APPROVE_DISCRIMINATOR,
    )
    .map_err(|e| {
        eyre::eyre!(
            "Failed to create approve {} transaction message: {}",
            kind_for_label.label(),
            e
        )
    })?;

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(approve_msg),
        &[fee_payer_signer.as_ref()],
    )?;

    // A config change is disclosed in full and confirmed on its own terms; a
    // feature gate action keeps the routine send prompt.
    // Select the disclosure from the on-chain account type, not from `flavor`,
    // which is the unverified `--kind` the caller passed. Approving a config
    // transaction as `--kind activate` previously took the vault arm and its
    // member and threshold changes were never shown.
    let confirmed = match read_config_transaction(&rpc_client, &multisig_address, proposal_index)? {
        Some(config_tx) => confirm_config_change(
            &rpc_client,
            &multisig_address,
            proposal_index,
            &config_tx,
            "Approve",
        )?,
        None => confirm_action("Send this approve transaction now?", true),
    };
    if !confirmed {
        output::Output::hint(
            "Skipped sending. You can submit the above encoded transaction manually or rerun to send.",
        );
        return Ok(());
    }

    let signature = crate::provision::send_and_confirm_transaction(&transaction, &rpc_client)
        .map_err(|e| eyre::eyre!("Failed to send approval transaction: {}", e))?;

    let success_label = match flavor {
        ProposalFlavor::Vault(_) => "Transaction sent successfully:",
        ProposalFlavor::Config => "Config change approved (sig):",
    };
    output::Output::field(success_label, &signature);
    Ok(())
}

pub fn execute_common_config_change(
    config: &Config,
    multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    transaction_index: u64,
) -> Result<()> {
    let fee_payer_signer = load_signer(&fee_payer_path, "fee payer")?;

    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = create_rpc_client(&rpc_url);

    // Readiness check: ensure approvals >= threshold on config proposal
    let (approved, threshold, _status) =
        get_proposal_status_and_threshold(&multisig_address, transaction_index, &rpc_client)?;
    if (approved as u16) < threshold {
        output::Output::hint(&format!(
            "Config proposal at index {} not ready to execute yet: approvals {}/{}",
            transaction_index, approved, threshold
        ));
        return Err(eyre::eyre!("Insufficient approvals to execute"));
    }

    let is_parent_multisig = load_multisig_if_any(&rpc_client, &voting_key)?.is_some();

    if is_parent_multisig {
        return handle_parent_multisig_flow(
            voting_key,
            multisig_address,
            transaction_index,
            TransactionKind::Rekey,
            ChildTransactionFlavor::Config,
            &fee_payer_signer,
            &rpc_url,
            ProposalAction::Execute,
            ParentFlowPayload::None,
        );
    }

    // Permission check: voting_key must be a member with Execute permission on the multisig
    let multisig = fetch_squads_multisig(&rpc_client, &multisig_address, "multisig")?;

    verify_member_permission(&multisig, &voting_key, PERMISSION_EXECUTE, "Executor")?;
    require_eoa_voting_key_matches_fee_payer(&voting_key, &fee_payer_signer.pubkey())?;

    output::Output::header("Config Execute - Signer Details");
    output::Output::field("Fee payer", &fee_payer_signer.pubkey().to_string());
    output::Output::field("Voting key (member)", &voting_key.to_string());

    // Fresh blockhash and build execute message for the config transaction
    let blockhash = rpc_client.get_latest_blockhash()?;
    let exec_msg = create_execute_config_transaction_message(
        &multisig_address,
        &voting_key,
        &fee_payer_signer.pubkey(),
        Some(fee_payer_signer.pubkey()),
        transaction_index,
        blockhash,
    )?;

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(exec_msg),
        &[fee_payer_signer.as_ref()],
    )?;

    // Execution is the irreversible step, so disclose the full effect here too
    // rather than relying on whatever was shown at approval time.
    let Some(config_tx) =
        read_config_transaction(&rpc_client, &multisig_address, transaction_index)?
    else {
        return Err(eyre::eyre!(
            "Proposal #{transaction_index} is not a config transaction; refusing to execute it as one."
        ));
    };
    if !confirm_config_change(
        &rpc_client,
        &multisig_address,
        transaction_index,
        &config_tx,
        "Execute",
    )? {
        output::Output::hint("Skipped sending. Rerun to send.");
        return Ok(());
    }

    // Send and confirm
    let signature = crate::provision::send_and_confirm_transaction(&transaction, &rpc_client)
        .map_err(|e| eyre::eyre!("Failed to execute config change: {}", e))?;
    output::Output::field("Config change executed (sig)", &signature);
    Ok(())
}
