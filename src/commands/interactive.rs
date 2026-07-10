use crate::commands::{
    config_command, create_command, proposal_command, show_command, verify_command,
    ProposalCommand, ProposalCommandArgs, TransactionKind,
};
use crate::feature_gate_program::FEATURE_GATE_PROGRAM_ID;
use crate::output::Output;
use crate::provision::{create_rpc_client, fetch_squads_multisig};
use crate::squads::{
    deserialize_squads_account, get_proposal_pda, get_transaction_pda, get_vault_pda,
    ConfigTransaction, Proposal, ProposalStatus, VaultTransaction,
    CONFIG_TRANSACTION_ACCOUNT_DISCRIMINATOR, PROPOSAL_ACCOUNT_DISCRIMINATOR,
    SQUADS_MULTISIG_PROGRAM_ID, VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
};
use crate::utils::*;
use eyre::Result;
use inquire::{Confirm, Select, Text};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;

pub fn interactive_mode() -> Result<()> {
    let mut config = load_config()?;

    loop {
        let options = vec![
            "Create new feature gate multisig",
            "Show feature gate multisig details",
            "Verify feature gate multisig",
            "Show configuration",
            "Proposal Actions (Approve/Reject/Execute)",
            "Exit",
        ];

        let choice: &str = Select::new("What would you like to do?", options).prompt()?;

        match choice {
            "Create new feature gate multisig" => {
                let feepayer_path = prompt_for_fee_payer_path(&config)?;
                create_command(&mut config, None, Some(feepayer_path))?;
            }
            "Proposal Actions (Approve/Reject/Execute)" => {
                handle_proposal_action(&config)?;
            }
            "Show feature gate multisig details" => {
                let address = Text::new("Enter the main multisig address:").prompt()?;
                show_command(&config, Some(address))?;
            }
            "Verify feature gate multisig" => {
                let address = Text::new("Enter the feature gate multisig address:").prompt()?;
                verify_command(&config, Some(address))?;
            }
            "Show configuration" => {
                config_command(&config)?;
            }
            "Exit" => break,
            _ => unreachable!(),
        }

        println!("\n");
    }

    Ok(())
}

fn handle_proposal_action(config: &Config) -> Result<()> {
    println!("In the current setup, only the fee payer keypair is used for transactions, hence for EOA voting fee payer = voting account. For multisig setup, the fee payer needs to be a member of the parent multisig.\n");

    let multisig = prompt_for_pubkey("Enter the feature gate multisig address:")?;
    let feature_gate_id = get_vault_pda(&multisig, 0).0;

    // Pick the network once up front: the proposal picker queries it, and the
    // single-network config makes the downstream flows skip their own prompt.
    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = create_rpc_client(&rpc_url);
    let config = Config {
        networks: vec![rpc_url],
        ..config.clone()
    };
    let config = &config;

    let action_options = vec!["Create", "Approve", "Reject", "Execute", "Cancel"];
    let action_choice: &str = Select::new("Select action:", action_options).prompt()?;
    if action_choice == "Cancel" {
        return Ok(());
    }

    // Create picks a kind (nothing exists to infer from); the other actions pick
    // a live proposal, and its kind comes from the on-chain transaction shape.
    let (command, kind, index) = if action_choice == "Create" {
        let Some(kind) = prompt_for_transaction_kind()? else {
            return Ok(());
        };
        (ProposalCommand::Propose, kind, None)
    } else {
        let command = match action_choice {
            "Approve" => ProposalCommand::Approve,
            "Reject" => ProposalCommand::Reject,
            "Execute" => ProposalCommand::Execute,
            _ => unreachable!(),
        };

        let (index, proposal_kind) = prompt_for_proposal_index(&rpc_client, &multisig, command)?;
        let kind = match proposal_kind.transaction_kind() {
            Some(kind) => kind,
            // Not a shape this tool creates; the user has to say how to route it.
            None => {
                Output::warning(
                    "Could not classify this proposal from its on-chain transaction; select its type:",
                );
                let Some(kind) = prompt_for_transaction_kind()? else {
                    return Ok(());
                };
                kind
            }
        };

        let confirmed = Confirm::new(&format!(
            "You're about to {} proposal #{} ({}) on feature gate {}. Continue?",
            action_choice.to_lowercase(),
            index,
            proposal_kind.label(),
            feature_gate_id,
        ))
        .with_default(true)
        .prompt()?;
        if !confirmed {
            return Ok(());
        }

        (command, kind, Some(index))
    };

    // Same entry point as the CLI subcommands: fee payer and voting key resolve
    // from saved config defaults, prompting only when absent.
    proposal_command(
        config,
        command,
        ProposalCommandArgs {
            multisig: multisig.to_string(),
            kind,
            voting_key: None,
            keypair: None,
            index,
        },
    )
}

/// Ask which kind of transaction to work with. Returns None on cancel.
fn prompt_for_transaction_kind() -> Result<Option<TransactionKind>> {
    let options = vec![
        "Activate Feature Gate",
        "Revoke Feature Gate",
        "Rekey Multisig (this will brick the multisig)",
        "Cancel",
    ];
    let choice: &str = Select::new("Select transaction type:", options).prompt()?;
    Ok(match choice {
        "Activate Feature Gate" => Some(TransactionKind::Activate),
        "Revoke Feature Gate" => Some(TransactionKind::Revoke),
        "Rekey Multisig (this will brick the multisig)" => Some(TransactionKind::Rekey),
        _ => None,
    })
}

/// What a proposal's companion transaction does, classified from its on-chain
/// shape. The tool only creates three forms: activate (System-program
/// allocate+assign), revoke (an instruction to the Feature Gate program), and
/// rekey (a config transaction).
#[derive(Clone, Copy)]
enum ProposalKind {
    Activate,
    Revoke,
    Rekey,
    ChildAction,
    Other,
    Unknown,
}

impl ProposalKind {
    fn label(self) -> &'static str {
        match self {
            ProposalKind::Activate => "Activate feature gate",
            ProposalKind::Revoke => "Revoke feature gate",
            ProposalKind::Rekey => "Rekey (config change)",
            ProposalKind::ChildAction => "Child multisig action",
            ProposalKind::Other => "Vault transaction",
            ProposalKind::Unknown => "Unknown",
        }
    }

    fn transaction_kind(self) -> Option<TransactionKind> {
        match self {
            ProposalKind::Activate => Some(TransactionKind::Activate),
            ProposalKind::Revoke => Some(TransactionKind::Revoke),
            ProposalKind::Rekey => Some(TransactionKind::Rekey),
            ProposalKind::ChildAction | ProposalKind::Other | ProposalKind::Unknown => None,
        }
    }
}

fn describe_transaction(rpc_client: &RpcClient, multisig: &Pubkey, index: u64) -> ProposalKind {
    let (transaction_pda, _) = get_transaction_pda(multisig, index);
    let Ok(account) = rpc_client.get_account(&transaction_pda) else {
        return ProposalKind::Unknown;
    };

    if deserialize_squads_account::<ConfigTransaction>(
        &account.data,
        CONFIG_TRANSACTION_ACCOUNT_DISCRIMINATOR,
        "config transaction",
    )
    .is_ok()
    {
        return ProposalKind::Rekey;
    }

    let Ok(vault_tx) = deserialize_squads_account::<VaultTransaction>(
        &account.data,
        VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
        "vault transaction",
    ) else {
        return ProposalKind::Unknown;
    };

    let programs: Vec<&Pubkey> = vault_tx
        .message
        .instructions
        .iter()
        .filter_map(|ix| {
            vault_tx
                .message
                .account_keys
                .get(ix.program_id_index as usize)
        })
        .collect();

    if programs.iter().any(|p| **p == FEATURE_GATE_PROGRAM_ID) {
        ProposalKind::Revoke
    } else if programs
        .iter()
        .all(|p| **p == solana_system_interface::program::ID)
    {
        ProposalKind::Activate
    } else if programs.iter().any(|p| **p == SQUADS_MULTISIG_PROGRAM_ID) {
        ProposalKind::ChildAction
    } else {
        ProposalKind::Other
    }
}

fn proposal_status_label(status: &ProposalStatus) -> &'static str {
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

/// Whether `action` can still be performed on a proposal, given its status and
/// staleness. Voting (approve/reject) requires an Active proposal that is not
/// stale. Execution requires an Approved proposal; a stale but approved vault
/// transaction can still execute, while a stale config transaction cannot.
fn is_actionable(
    action: ProposalCommand,
    status: &ProposalStatus,
    kind: ProposalKind,
    index: u64,
    stale_index: u64,
) -> bool {
    match action {
        ProposalCommand::Approve | ProposalCommand::Reject => {
            matches!(status, ProposalStatus::Active { .. }) && index > stale_index
        }
        ProposalCommand::Execute => {
            matches!(status, ProposalStatus::Approved { .. })
                && (index > stale_index || !matches!(kind, ProposalKind::Rekey))
        }
        ProposalCommand::Propose => true,
    }
}

/// List the multisig's proposals that `action` can still act on — with what
/// they do, their status, and vote counts — and let the user pick one instead
/// of typing an index from memory. Executed, rejected, cancelled, and stale
/// proposals are skipped (with a note). Falls back to manual entry when
/// nothing is listable or the user prefers. Returns the chosen index and its
/// classified kind.
fn prompt_for_proposal_index(
    rpc_client: &RpcClient,
    multisig: &Pubkey,
    action: ProposalCommand,
) -> Result<(u64, ProposalKind)> {
    const MAX_LISTED: u64 = 10;

    let mut entries: Vec<(u64, ProposalKind)> = Vec::new();
    let mut options: Vec<String> = Vec::new();
    let mut skipped = 0u64;
    match fetch_squads_multisig(rpc_client, multisig, "multisig") {
        Ok(ms) if ms.transaction_index > 0 => {
            let first = ms.transaction_index.saturating_sub(MAX_LISTED - 1).max(1);
            for index in first..=ms.transaction_index {
                let (proposal_pda, _) = get_proposal_pda(multisig, index);
                let Ok(account) = rpc_client.get_account(&proposal_pda) else {
                    continue;
                };
                let Ok(proposal) = deserialize_squads_account::<Proposal>(
                    &account.data,
                    PROPOSAL_ACCOUNT_DISCRIMINATOR,
                    "proposal",
                ) else {
                    continue;
                };
                let kind = describe_transaction(rpc_client, multisig, index);
                if !is_actionable(
                    action,
                    &proposal.status,
                    kind,
                    index,
                    ms.stale_transaction_index,
                ) {
                    skipped += 1;
                    continue;
                }
                entries.push((index, kind));
                options.push(format!(
                    "#{index} — {} — {} — {} approval(s), {} rejection(s)",
                    kind.label(),
                    proposal_status_label(&proposal.status),
                    proposal.approved.len(),
                    proposal.rejected.len(),
                ));
            }
        }
        Ok(_) => {}
        Err(e) => Output::warning(&format!("Could not list proposals: {e}")),
    }

    if skipped > 0 {
        Output::info(&format!(
            "Skipped {skipped} proposal(s) this action can no longer apply to (executed/rejected/cancelled/stale)."
        ));
    }

    if !options.is_empty() {
        options.push("Enter an index manually".to_string());
        let selection = Select::new("Select a proposal:", options).raw_prompt()?;
        if selection.index < entries.len() {
            return Ok(entries[selection.index]);
        }
    } else {
        Output::info("No proposals are currently actionable for this action.");
    }

    let index_str = Text::new("Enter the proposal index:").prompt()?;
    let index: u64 = index_str
        .parse()
        .map_err(|_| eyre::eyre!("Invalid proposal index"))?;
    Ok((index, describe_transaction(rpc_client, multisig, index)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionability_matrix() {
        let active = ProposalStatus::Active { timestamp: 0 };
        let approved = ProposalStatus::Approved { timestamp: 0 };
        let executed = ProposalStatus::Executed { timestamp: 0 };
        let rejected = ProposalStatus::Rejected { timestamp: 0 };
        let cancelled = ProposalStatus::Cancelled { timestamp: 0 };

        // Voting works only on Active, non-stale proposals.
        for action in [ProposalCommand::Approve, ProposalCommand::Reject] {
            assert!(is_actionable(action, &active, ProposalKind::Activate, 5, 2));
            // stale (index <= stale_index)
            assert!(!is_actionable(
                action,
                &active,
                ProposalKind::Activate,
                2,
                2
            ));
            for status in [&approved, &executed, &rejected, &cancelled] {
                assert!(!is_actionable(action, status, ProposalKind::Activate, 5, 2));
            }
        }

        // Execute requires Approved; terminal states are out.
        assert!(is_actionable(
            ProposalCommand::Execute,
            &approved,
            ProposalKind::Activate,
            5,
            2
        ));
        for status in [&active, &executed, &rejected, &cancelled] {
            assert!(!is_actionable(
                ProposalCommand::Execute,
                status,
                ProposalKind::Activate,
                5,
                2
            ));
        }

        // Stale exception: an approved vault transaction can still execute,
        // but a stale config (rekey) transaction cannot.
        assert!(is_actionable(
            ProposalCommand::Execute,
            &approved,
            ProposalKind::Revoke,
            2,
            2
        ));
        assert!(!is_actionable(
            ProposalCommand::Execute,
            &approved,
            ProposalKind::Rekey,
            2,
            2
        ));
    }
}
