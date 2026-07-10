//! Non-interactive proposal subcommands: `propose`, `approve`, `reject`,
//! `execute`. Thin argument-resolution wrappers over the same flow functions
//! the interactive menu uses, so signers can act from a runbook one-liner.

use crate::commands::transaction_generation::{
    approve_common_config_change, approve_common_feature_gate_proposal,
    create_feature_gate_proposal, execute_common_config_change,
    execute_common_feature_gate_proposal, reject_common_feature_gate_proposal,
    rekey_multisig_feature_gate, TransactionKind,
};
use crate::output::Output;
use crate::utils::{
    is_assume_yes, is_e2e_test_mode, prompt_for_fee_payer_path, prompt_for_pubkey, save_config,
    Config,
};
use eyre::Result;
use solana_pubkey::Pubkey;
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
    let voting_key = resolve_voting_key(config, args.voting_key.as_deref())?;
    let fee_payer_path = match args.keypair {
        Some(path) => path,
        None => prompt_for_fee_payer_path(config)?,
    };

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
