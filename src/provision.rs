use crate::constants::*;
use crate::squads::{
    get_multisig_pda, get_program_config_pda, get_proposal_pda, get_transaction_pda, get_vault_pda,
    CompiledInstruction, InstructionData, Member, MultisigCreateArgsV2,
    MultisigCreateProposalAccounts, MultisigCreateProposalArgs, MultisigCreateProposalData,
    MultisigCreateTransaction, MultisigCreateV2Accounts, MultisigCreateV2Data,
    MultisigExecuteTransactionAccounts, MultisigExecuteTransactionArgs,
    MultisigVoteOnProposalAccounts, MultisigVoteOnProposalArgs, ProgramConfig, SmallVec,
    TransactionMessage, VaultTransactionCreateArgs, VaultTransactionCreateArgsData,
    CONFIG_TRANSACTION_EXECUTE_DISCRIMINATOR, EXECUTE_TRANSACTION_DISCRIMINATOR,
    SQUADS_MULTISIG_PROGRAM_ID,
};

use crate::utils::{decode_permissions, get_network_display};
use borsh::BorshDeserialize;
use colored::Colorize;
use eyre::eyre;
use indicatif::ProgressBar;
use inquire::Confirm;
use solana_client::client_error::ClientErrorKind;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_client::rpc_request::{RpcError, RpcResponseErrorData};
use solana_client::rpc_response::RpcSimulateTransactionResult;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::v0::Message;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::str::FromStr;
use std::time::Duration;

/// Creates an RPC client with consistent commitment configuration
pub fn create_rpc_client(url: &str) -> RpcClient {
    RpcClient::new_with_commitment(url, CommitmentConfig::confirmed())
}

#[derive(Clone, Copy)]
struct MessageAccount {
    pubkey: Pubkey,
    is_signer: bool,
    is_writable: bool,
}

impl MessageAccount {
    fn matches_class(self, is_signer: bool, is_writable: bool) -> bool {
        self.is_signer == is_signer && self.is_writable == is_writable
    }
}

fn upsert_message_account(accounts: &mut Vec<MessageAccount>, meta: AccountMeta) {
    if let Some(account) = accounts
        .iter_mut()
        .find(|account| account.pubkey == meta.pubkey)
    {
        account.is_signer |= meta.is_signer;
        account.is_writable |= meta.is_writable;
    } else {
        accounts.push(MessageAccount {
            pubkey: meta.pubkey,
            is_signer: meta.is_signer,
            is_writable: meta.is_writable,
        });
    }
}

/// Convert a list of Instructions into a Squads TransactionMessage by deterministically
/// deduplicating accounts, then computing the header counts.
///
/// This approach:
/// 1. Records accounts in first-seen order, including program IDs
/// 2. Deduplicates accounts while upgrading permissions for duplicates
/// 3. Stable-partitions accounts into Solana's static account classes
/// 4. Computes header counts based on the final sorted list
///
/// The `_payer` parameter is unused but kept for API compatibility - Squads uses the
/// vault PDA as the actual signer which is derived from the instruction accounts.
pub fn build_squads_transaction_message(
    instructions: &[Instruction],
    _payer: &Pubkey,
) -> eyre::Result<TransactionMessage> {
    let mut accounts = Vec::new();
    for ix in instructions {
        for meta in &ix.accounts {
            upsert_message_account(&mut accounts, meta.clone());
        }
        upsert_message_account(
            &mut accounts,
            AccountMeta::new_readonly(ix.program_id, false),
        );
    }

    let writable_signers: Vec<_> = accounts
        .iter()
        .copied()
        .filter(|account| account.matches_class(true, true))
        .collect();
    let readonly_signers: Vec<_> = accounts
        .iter()
        .copied()
        .filter(|account| account.matches_class(true, false))
        .collect();
    let writable_non_signers: Vec<_> = accounts
        .iter()
        .copied()
        .filter(|account| account.matches_class(false, true))
        .collect();
    let readonly_non_signers: Vec<_> = accounts
        .iter()
        .copied()
        .filter(|account| account.matches_class(false, false))
        .collect();

    let num_writable_signers = writable_signers.len() as u8;
    let num_signers = (writable_signers.len() + readonly_signers.len()) as u8;
    let num_writable_non_signers = writable_non_signers.len() as u8;

    let account_keys: Vec<Pubkey> = writable_signers
        .into_iter()
        .chain(readonly_signers)
        .chain(writable_non_signers)
        .chain(readonly_non_signers)
        .map(|account| account.pubkey)
        .collect();

    // Convert instructions to Squads CompiledInstruction format
    let compiled_instructions: Vec<CompiledInstruction> = instructions
        .iter()
        .map(|ix| {
            let program_id_index = account_keys
                .iter()
                .position(|key| *key == ix.program_id)
                .map(|index| index as u8)
                .ok_or_else(|| eyre::eyre!("program_id {} not in account_keys", ix.program_id))?;
            let account_indexes: Vec<u8> = ix
                .accounts
                .iter()
                .map(|meta| {
                    account_keys
                        .iter()
                        .position(|key| *key == meta.pubkey)
                        .map(|index| index as u8)
                        .ok_or_else(|| eyre::eyre!("account {} not in account_keys", meta.pubkey))
                })
                .collect::<eyre::Result<Vec<u8>>>()?;

            Ok(CompiledInstruction {
                program_id_index,
                account_indexes: SmallVec::from(account_indexes),
                data: SmallVec::from(ix.data.clone()),
            })
        })
        .collect::<eyre::Result<Vec<CompiledInstruction>>>()?;

    Ok(TransactionMessage {
        num_signers,
        num_writable_signers,
        num_writable_non_signers,
        account_keys: SmallVec::from(account_keys),
        instructions: SmallVec::from(compiled_instructions),
        address_table_lookups: SmallVec::from(vec![]),
    })
}

pub fn send_and_confirm_transaction(
    transaction: &VersionedTransaction,
    rpc_client: &RpcClient,
) -> eyre::Result<String> {
    const MAX_RETRIES: usize = MAX_TX_RETRIES;
    const BASE_DELAY_MS: u64 = BASE_RETRY_DELAY_MS;
    const MAX_TOTAL_RETRY_TIME_MS: u64 = 10_000; // 10 seconds total

    let mut last_error: Option<eyre::Report> = None;
    let retry_start = std::time::Instant::now();

    for attempt in 0..MAX_RETRIES {
        // Check if we've exceeded our total retry time budget
        if retry_start.elapsed().as_millis() as u64 >= MAX_TOTAL_RETRY_TIME_MS {
            println!(
                "Exceeded maximum retry time of {}ms",
                MAX_TOTAL_RETRY_TIME_MS
            );
            break;
        }

        if attempt > 0 {
            let delay = BASE_DELAY_MS * (2_u64.pow(attempt as u32 - 1));
            // Ensure we don't exceed our total time budget with this delay
            let remaining_time =
                MAX_TOTAL_RETRY_TIME_MS.saturating_sub(retry_start.elapsed().as_millis() as u64);
            let actual_delay = std::cmp::min(delay, remaining_time);

            if actual_delay > 0 {
                println!(
                    "Retrying transaction in {}ms... (attempt {}/{}, {}ms elapsed)",
                    actual_delay,
                    attempt + 1,
                    MAX_RETRIES,
                    retry_start.elapsed().as_millis()
                );
                std::thread::sleep(Duration::from_millis(actual_delay));
            } else {
                println!(
                    "No time remaining for delay, proceeding with retry attempt {}/{}",
                    attempt + 1,
                    MAX_RETRIES
                );
            }
        }

        // First try to send the transaction
        let signature = match rpc_client.send_transaction_with_config(
            transaction,
            RpcSendTransactionConfig {
                skip_preflight: false,
                preflight_commitment: Some(rpc_client.commitment().commitment),
                encoding: None,
                max_retries: Some(0), // We handle retries ourselves
                min_context_slot: None,
            },
        ) {
            Ok(sig) => sig,
            Err(err) => {
                // Check if this is a retryable error
                let is_retryable = match &err.kind {
                    ClientErrorKind::RpcError(RpcError::RpcResponseError { code, .. }) => {
                        // Common retryable RPC errors
                        *code == -32005 ||  // Node is unhealthy
                        *code == -32004 ||  // RPC request timed out
                        *code == -32603 ||  // Internal error
                        *code == -32002 ||  // Transaction simulation failed
                        *code == -32001 // Generic server error
                    }
                    ClientErrorKind::Io(_) => true, // Network issues
                    ClientErrorKind::Reqwest(_) => true, // HTTP client issues
                    _ => false,
                };

                if let ClientErrorKind::RpcError(RpcError::RpcResponseError {
                    data:
                        RpcResponseErrorData::SendTransactionPreflightFailure(
                            RpcSimulateTransactionResult {
                                logs: Some(logs), ..
                            },
                        ),
                    ..
                }) = &err.kind
                {
                    println!("Simulation logs:\n\n{}\n", logs.join("\n").bright_yellow());
                }

                last_error = Some(eyre::eyre!("{}", err));

                // Don't retry on the last attempt or if error is not retryable
                if attempt == MAX_RETRIES - 1 || !is_retryable {
                    break;
                }

                println!(
                    "Retryable error occurred: {}",
                    last_error
                        .as_ref()
                        .map(|e| e.to_string())
                        .unwrap_or_default()
                        .bright_yellow()
                );
                continue;
            }
        };

        // Now wait for confirmation with exponential backoff polling
        let confirmation_start = std::time::Instant::now();
        let mut confirmation_poll_delay = CONFIRMATION_POLL_INTERVAL_MS;

        loop {
            if confirmation_start.elapsed().as_millis() as u64 > CONFIRMATION_TIMEOUT_MS {
                println!(
                    "Transaction confirmation timeout after {}ms",
                    CONFIRMATION_TIMEOUT_MS
                );
                break; // Will retry sending
            }

            match rpc_client.get_signature_status(&signature) {
                Ok(Some(Ok(()))) => {
                    return Ok(signature.to_string());
                }
                Ok(Some(Err(_))) => {
                    // Transaction failed
                    println!("Transaction failed confirmation");
                    break;
                }
                Ok(None) => {
                    // Transaction not yet confirmed, continue polling
                }
                Err(confirmation_err) => {
                    // Check if confirmation error is retryable
                    match &confirmation_err.kind {
                        ClientErrorKind::RpcError(RpcError::RpcResponseError { code, .. }) => {
                            if *code == -32004 || *code == -32005 || *code == -32603 {
                                // Temporary RPC issue, continue polling
                                println!(
                                    "Temporary confirmation error: {}",
                                    confirmation_err.to_string().bright_yellow()
                                );
                            } else {
                                // Non-retryable confirmation error, break and retry transaction
                                println!(
                                    "Non-retryable confirmation error: {}",
                                    confirmation_err.to_string().bright_red()
                                );
                                break;
                            }
                        }
                        ClientErrorKind::Io(_) | ClientErrorKind::Reqwest(_) => {
                            // Network issues, continue polling
                            println!(
                                "Network error during confirmation: {}",
                                confirmation_err.to_string().bright_yellow()
                            );
                        }
                        _ => {
                            // Unknown error, break and retry transaction
                            println!(
                                "Unknown confirmation error: {}",
                                confirmation_err.to_string().bright_red()
                            );
                            break;
                        }
                    }
                }
            }

            // Wait before next confirmation check with exponential backoff (capped at 5 seconds)
            std::thread::sleep(Duration::from_millis(confirmation_poll_delay));
            confirmation_poll_delay = std::cmp::min(confirmation_poll_delay * 2, 5000);
        }

        // If we reach here, confirmation failed or timed out
        last_error = Some(eyre!(
            "Transaction sent but confirmation failed or timed out"
        ));
    }

    Err(eyre!(
        "Transaction failed after {} attempts: {}",
        MAX_RETRIES,
        last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "Unknown error".to_string())
    ))
}

pub fn get_account_data_with_retry(
    rpc_client: &RpcClient,
    pubkey: &Pubkey,
) -> eyre::Result<Vec<u8>> {
    const MAX_RETRIES: usize = MAX_ACCOUNT_RETRIES;
    const BASE_DELAY_MS: u64 = BASE_ACCOUNT_RETRY_DELAY_MS;

    let mut last_error = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = BASE_DELAY_MS * (2_u64.pow(attempt as u32 - 1));
            std::thread::sleep(Duration::from_millis(delay));
        }

        match rpc_client.get_account_data(pubkey) {
            Ok(data) => return Ok(data),
            Err(err) => {
                let is_retryable = match &err.kind {
                    ClientErrorKind::RpcError(RpcError::RpcResponseError { code, .. }) => {
                        *code == -32005 || *code == -32004 || *code == -32603
                    }
                    ClientErrorKind::Io(_) => true,
                    ClientErrorKind::Reqwest(_) => true,
                    _ => false,
                };

                last_error = Some(err);

                if attempt == MAX_RETRIES - 1 || !is_retryable {
                    break;
                }
            }
        }
    }

    Err(eyre!(
        "Failed to get account data after {} attempts: {}",
        MAX_RETRIES,
        last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "Unknown error".to_string())
    ))
}
pub async fn create_multisig(
    rpc_url: String,
    program_id: Option<String>,
    fee_payer_keypair: &dyn Signer,
    create_key: &Keypair,
    members: Vec<Member>,
    threshold: u16,
    priority_fee_lamports: Option<u64>,
) -> eyre::Result<(Pubkey, String)> {
    let program_id = program_id.unwrap_or_else(|| SQUADS_PROGRAM_ID_STR.to_string());
    let program_id = Pubkey::from_str(&program_id)
        .map_err(|e| eyre::eyre!("Invalid program ID '{}': {}", program_id, e))?;
    let multisig_address = crate::squads::get_multisig_pda(&create_key.pubkey(), None).0;
    let vault_address = get_vault_pda(&multisig_address, 0, None).0;

    let transaction_creator = fee_payer_keypair.pubkey();

    println!();
    println!(
        "{}",
        "👀 Review Feature Gate Multisig Details"
            .bright_yellow()
            .bold()
    );
    println!();
    println!("{}: {}", "Network".cyan(), rpc_url.bright_white());
    println!(
        "{}: {}",
        "Program ID".cyan(),
        program_id.to_string().bright_white()
    );
    println!(
        "{}: {}",
        "Fee Payer".cyan(),
        transaction_creator.to_string().bright_white()
    );
    println!();
    println!("{}", "⚙️ General Info".bright_white().bold());
    println!();
    println!(
        "{}: {}",
        "Feature Gate Multisig".cyan(),
        multisig_address.to_string().bright_white()
    );
    println!(
        "{}: {}",
        "Feature Gate ID".cyan(),
        vault_address.to_string().bright_white()
    );
    println!();
    println!("{}", "⚙️ Config Parameters".bright_white().bold());
    println!();
    println!(
        "{}: {}",
        "Members".cyan(),
        members.len().to_string().bright_green()
    );
    for (i, member) in members.iter().enumerate() {
        let perms = decode_permissions(member.permissions.mask);
        if perms.len() == 1 && perms[0] == "Initiate" {
            println!(
                "  {} Temporary Setup Keypair: {} ({})",
                "✓".bright_green(),
                member.key.to_string().bright_white(),
                "Initiate".bright_cyan()
            );
        } else {
            println!(
                "  {} Member {}: {} ({})",
                "✓".bright_green(),
                i + 1,
                member.key.to_string().bright_white(),
                perms.join(", ").bright_cyan()
            );
        }
    }
    println!("");
    println!(
        "{}: {}",
        "Threshold".cyan(),
        threshold.to_string().bright_green()
    );
    println!();

    let proceed = if std::env::var("E2E_TEST_MODE").is_ok() {
        true
    } else {
        Confirm::new("Do you want to proceed?")
            .with_default(true)
            .prompt()?
    };
    if !proceed {
        println!("{}", "OK, aborting.".bright_red());
        return Err(eyre!("User aborted"));
    }
    println!();

    let rpc_client = create_rpc_client(&rpc_url);

    let progress = ProgressBar::new_spinner().with_message("Sending transactions...");
    progress.enable_steady_tick(Duration::from_millis(100));

    let blockhash = rpc_client
        .get_latest_blockhash()
        .map_err(|e| eyre::eyre!("Failed to get blockhash: {}", e))?;

    let multisig_key = get_multisig_pda(&create_key.pubkey(), Some(&program_id));

    let program_config_pda = get_program_config_pda(Some(&program_id));

    let program_config = rpc_client
        .get_account(&program_config_pda.0)
        .map_err(|e| eyre::eyre!("Failed to fetch program config account: {}", e))?;

    let program_config_data = program_config.data.as_slice();

    // Skip the first 8 bytes (discriminator) before deserializing
    if program_config_data.len() < 8 {
        return Err(eyre::eyre!(
            "Program config account data too small: {} bytes (expected at least 8)",
            program_config_data.len()
        ));
    }
    let config_data_without_discriminator = &program_config_data[8..];

    let treasury = borsh::from_slice::<ProgramConfig>(config_data_without_discriminator)
        .map_err(|e| eyre::eyre!("Failed to deserialize program config: {}", e))?
        .treasury;

    let message = Message::try_compile(
        &transaction_creator,
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(CREATE_MULTISIG_COMPUTE_UNITS),
            ComputeBudgetInstruction::set_compute_unit_price(
                priority_fee_lamports.unwrap_or(DEFAULT_PRIORITY_FEE),
            ),
            Instruction {
                accounts: MultisigCreateV2Accounts {
                    create_key: create_key.pubkey(),
                    creator: transaction_creator,
                    multisig: multisig_key.0,
                    system_program: solana_system_interface::program::ID,
                    program_config: program_config_pda.0,
                    treasury,
                }
                .to_account_metas(),
                data: MultisigCreateV2Data {
                    args: MultisigCreateArgsV2 {
                        config_authority: None,
                        members,
                        threshold,
                        time_lock: 0,
                        memo: None,
                        rent_collector: None,
                    },
                }
                .data()?,
                program_id,
            },
        ],
        &[],
        blockhash,
    )
    .map_err(|e| eyre::eyre!("Failed to compile message: {}", e))?;

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(message),
        &[fee_payer_keypair, create_key as &dyn Signer],
    )
    .map_err(|e| eyre::eyre!("Failed to create transaction: {}", e))?;

    let signature = send_and_confirm_transaction(&transaction, &rpc_client)?;

    let network_display = get_network_display(&rpc_url);

    progress.finish_with_message(format!(
        "Multisig creation confirmed: {} ({})",
        signature.to_string().bright_green(),
        network_display
    ));

    Ok((multisig_key.0, signature))
}

pub fn create_transaction_and_proposal_message(
    program_id: Option<&Pubkey>,
    fee_payer_pubkey: &Pubkey,
    contributor_pubkey: &Pubkey,
    multisig_address: &Pubkey,
    transaction_index: u64,
    vault_index: u8,
    transaction_message: TransactionMessage,
    priority_fee: Option<u32>,
    compute_unit_limit: Option<u32>,
    recent_blockhash: Hash,
) -> eyre::Result<(Message, Pubkey, Pubkey)> {
    let program_id = program_id.unwrap_or(&crate::squads::SQUADS_MULTISIG_PROGRAM_ID);

    let (transaction_pda, _transaction_bump) =
        get_transaction_pda(multisig_address, transaction_index, Some(program_id));
    let (proposal_pda, _proposal_bump) =
        get_proposal_pda(multisig_address, transaction_index, Some(program_id));

    let mut instructions = Vec::new();

    if let Some(microlamports) = priority_fee {
        instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
            microlamports as u64,
        ));
    }

    // Add compute unit limit if specified
    if let Some(units) = compute_unit_limit {
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(units));
    }

    instructions.push(create_vault_transaction_create_instruction(
        program_id,
        multisig_address,
        &transaction_pda,
        contributor_pubkey,
        fee_payer_pubkey,
        vault_index,
        transaction_message,
    )?);
    instructions.push(create_proposal_create_instruction(
        program_id,
        multisig_address,
        &proposal_pda,
        contributor_pubkey,
        fee_payer_pubkey,
        transaction_index,
    )?);

    let message = Message::try_compile(fee_payer_pubkey, &instructions, &[], recent_blockhash)?;

    Ok((message, transaction_pda, proposal_pda))
}

fn create_vault_transaction_create_instruction(
    program_id: &Pubkey,
    multisig_address: &Pubkey,
    transaction_pda: &Pubkey,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    vault_index: u8,
    transaction_message: TransactionMessage,
) -> eyre::Result<Instruction> {
    let accounts = MultisigCreateTransaction {
        multisig: *multisig_address,
        transaction: *transaction_pda,
        creator: *creator,
        rent_payer: *rent_payer,
        system_program: solana_system_interface::program::ID,
    };

    let data = VaultTransactionCreateArgsData {
        args: VaultTransactionCreateArgs {
            vault_index,
            ephemeral_signers: 0,
            transaction_message: borsh::to_vec(&transaction_message)?,
            memo: None,
        },
    };

    Ok(Instruction::new_with_bytes(
        *program_id,
        &data.data()?,
        accounts.to_account_metas(),
    ))
}

pub(crate) fn create_proposal_create_instruction(
    program_id: &Pubkey,
    multisig_address: &Pubkey,
    proposal_pda: &Pubkey,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    transaction_index: u64,
) -> eyre::Result<Instruction> {
    let accounts = MultisigCreateProposalAccounts {
        multisig: *multisig_address,
        proposal: *proposal_pda,
        creator: *creator,
        rent_payer: *rent_payer,
        system_program: solana_system_interface::program::ID,
    };

    let data = MultisigCreateProposalData {
        args: MultisigCreateProposalArgs {
            transaction_index,
            is_draft: false,
        },
    };

    Ok(Instruction::new_with_bytes(
        *program_id,
        &data.data()?,
        accounts.to_account_metas(),
    ))
}

/// Create a vote (approve or reject) message for a proposal.
/// Pass `PROPOSAL_APPROVE_DISCRIMINATOR` for approval or `PROPOSAL_REJECT_DISCRIMINATOR` for rejection.
pub fn create_vote_proposal_message(
    program_id: &Pubkey,
    multisig_address: &Pubkey,
    member_pubkey: &Pubkey,
    fee_payer_pubkey: &Pubkey,
    proposal_index: u64,
    recent_blockhash: Hash,
    discriminator: &[u8],
) -> eyre::Result<Message> {
    let (proposal_pda, _proposal_bump) =
        get_proposal_pda(multisig_address, proposal_index, Some(program_id));

    let account_keys = MultisigVoteOnProposalAccounts {
        multisig: *multisig_address,
        member: *member_pubkey,
        proposal: proposal_pda,
    };

    // Build instruction data: discriminator + serialized args
    let instruction_args = MultisigVoteOnProposalArgs { memo: None };
    let mut instruction_data = Vec::new();
    instruction_data.extend_from_slice(discriminator);
    instruction_data.extend_from_slice(&borsh::to_vec(&instruction_args)?);

    let vote_instruction = Instruction::new_with_bytes(
        *program_id,
        &instruction_data,
        account_keys.to_account_metas(),
    );

    let message =
        Message::try_compile(fee_payer_pubkey, &[vote_instruction], &[], recent_blockhash)?;

    Ok(message)
}

/// Fetch proposal approvals count, parent threshold, and proposal status for a given proposal index.
pub fn get_proposal_status_and_threshold(
    program_id: &Pubkey,
    multisig_address: &Pubkey,
    proposal_index: u64,
    rpc_client: &RpcClient,
) -> eyre::Result<(usize, u16, crate::squads::ProposalStatus)> {
    use crate::squads::{get_proposal_pda, Multisig as SquadsMultisig, Proposal};

    // Multisig threshold
    let ms_acc = rpc_client.get_account(multisig_address)?;
    let ms: SquadsMultisig = BorshDeserialize::deserialize(&mut &ms_acc.data[8..])
        .map_err(|e| eyre::eyre!("Failed to deserialize multisig: {}", e))?;

    // Proposal approved count and status
    let (proposal_pda, _) = get_proposal_pda(multisig_address, proposal_index, Some(program_id));
    let prop_acc = rpc_client.get_account(&proposal_pda)?;
    let prop: Proposal = BorshDeserialize::deserialize(&mut &prop_acc.data[8..])
        .map_err(|e| eyre::eyre!("Failed to deserialize proposal: {}", e))?;

    Ok((prop.approved.len(), ms.threshold, prop.status))
}

/// Build a TransactionMessage for a parent multisig to execute a child's proposal.
/// This creates a single instruction that calls Squads `ExecuteTransaction` on the child multisig,
/// with `member_pubkey` set to the parent vault PDA. The resulting TransactionMessage can
/// be embedded into a parent multisig transaction using `create_transaction_and_proposal_message`.
pub fn create_child_execute_transaction_message(
    child_multisig: Pubkey,
    child_tx_index: u64,
    parent_member_pubkey: Pubkey,
    child_transaction_accounts: Vec<solana_instruction::AccountMeta>,
) -> eyre::Result<TransactionMessage> {
    // Derive child's proposal and transaction PDAs
    let (proposal_pda, _) = get_proposal_pda(
        &child_multisig,
        child_tx_index,
        Some(&SQUADS_MULTISIG_PROGRAM_ID),
    );
    let (transaction_pda, _) = get_transaction_pda(
        &child_multisig,
        child_tx_index,
        Some(&SQUADS_MULTISIG_PROGRAM_ID),
    );

    // Construct the execute instruction for the child multisig
    let accounts = MultisigExecuteTransactionAccounts {
        multisig: child_multisig,
        proposal: proposal_pda,
        transaction: transaction_pda,
        member: parent_member_pubkey,
    };

    let instruction_args = MultisigExecuteTransactionArgs { memo: None };
    let mut instruction_data = Vec::new();
    instruction_data.extend_from_slice(EXECUTE_TRANSACTION_DISCRIMINATOR);
    instruction_data.extend_from_slice(
        &borsh::to_vec(&instruction_args)
            .map_err(|e| eyre::eyre!("Failed to serialize instruction args: {}", e))?,
    );

    // Build metas excluding header accounts (those will be added by accounts struct)
    let header_set: std::collections::HashSet<Pubkey> = [
        child_multisig,
        proposal_pda,
        transaction_pda,
        parent_member_pubkey,
    ]
    .into_iter()
    .collect();

    let filtered_dynamic_accounts: Vec<solana_instruction::AccountMeta> =
        child_transaction_accounts
            .into_iter()
            .filter(|m| !header_set.contains(&m.pubkey))
            .collect();

    let all_metas = accounts.to_account_metas(filtered_dynamic_accounts);

    let ix = Instruction {
        program_id: SQUADS_MULTISIG_PROGRAM_ID,
        accounts: all_metas,
        data: instruction_data,
    };

    // Use centralized helper - parent_member_pubkey is the signer (vault PDA)
    build_squads_transaction_message(&[ix], &parent_member_pubkey)
}

/// Build a TransactionMessage for a parent multisig to execute a child's config transaction.
/// Config transactions do not require extra execution metas beyond the base accounts.
pub fn create_child_execute_config_transaction_message(
    child_multisig: Pubkey,
    child_tx_index: u64,
    parent_member_pubkey: Pubkey,
) -> eyre::Result<TransactionMessage> {
    // Derive child's proposal and transaction PDAs
    let (proposal_pda, _) = get_proposal_pda(&child_multisig, child_tx_index, None);
    let (transaction_pda, _) = get_transaction_pda(&child_multisig, child_tx_index, None);

    // Build the account metas for ConfigTransactionExecute:
    // multisig (writable), member (signer), proposal (writable), transaction (readonly),
    // rent_payer (use program_id placeholder), system_program
    let account_metas = vec![
        AccountMeta::new(child_multisig, false),
        AccountMeta::new_readonly(parent_member_pubkey, true), // vault PDA as signer
        AccountMeta::new(proposal_pda, false),
        AccountMeta::new_readonly(transaction_pda, false),
        AccountMeta::new_readonly(SQUADS_MULTISIG_PROGRAM_ID, false), // rent_payer placeholder
        AccountMeta::new_readonly(solana_system_interface::program::ID, false),
    ];

    let instruction_data = CONFIG_TRANSACTION_EXECUTE_DISCRIMINATOR.to_vec();

    let ix = Instruction {
        program_id: SQUADS_MULTISIG_PROGRAM_ID,
        accounts: account_metas,
        data: instruction_data,
    };

    // Use centralized helper - parent_member_pubkey is the signer (vault PDA)
    build_squads_transaction_message(&[ix], &parent_member_pubkey)
}

/// Build a TransactionMessage for a parent multisig to CREATE a child's config transaction
/// and its proposal in a single parent transaction. The parent vault PDA acts as both
/// creator and rent payer on the child. The caller must ensure the parent vault is a member
/// of the child multisig with Initiate and Vote permissions.
pub fn create_child_create_config_transaction_and_proposal_message(
    child_multisig: Pubkey,
    child_tx_index: u64,
    parent_member_pubkey: Pubkey,
    rent_payer_pubkey: Pubkey,
    actions: Vec<crate::squads::ConfigAction>,
    memo: Option<String>,
) -> eyre::Result<TransactionMessage> {
    use crate::squads::SQUADS_MULTISIG_PROGRAM_ID;

    let (transaction_pda, _) = get_transaction_pda(
        &child_multisig,
        child_tx_index,
        Some(&SQUADS_MULTISIG_PROGRAM_ID),
    );
    let (proposal_pda, _) = get_proposal_pda(
        &child_multisig,
        child_tx_index,
        Some(&SQUADS_MULTISIG_PROGRAM_ID),
    );

    let config_create_instruction = create_config_transaction_create_instruction(
        &SQUADS_MULTISIG_PROGRAM_ID,
        &child_multisig,
        &transaction_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        actions,
        memo,
    )?;
    let create_proposal_instruction = create_proposal_create_instruction(
        &SQUADS_MULTISIG_PROGRAM_ID,
        &child_multisig,
        &proposal_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        child_tx_index,
    )?;

    build_squads_transaction_message(
        &[config_create_instruction, create_proposal_instruction],
        &parent_member_pubkey,
    )
}

pub(crate) fn create_config_transaction_create_instruction(
    program_id: &Pubkey,
    multisig_address: &Pubkey,
    transaction_pda: &Pubkey,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    actions: Vec<crate::squads::ConfigAction>,
    memo: Option<String>,
) -> eyre::Result<Instruction> {
    use crate::squads::{ConfigTransactionCreateArgs, ConfigTransactionCreateData};

    let accounts = vec![
        AccountMeta::new(*multisig_address, false),
        AccountMeta::new(*transaction_pda, false),
        AccountMeta::new_readonly(*creator, true),
        AccountMeta::new(*rent_payer, true),
        AccountMeta::new_readonly(solana_system_interface::program::ID, false),
    ];

    let data = ConfigTransactionCreateData {
        args: ConfigTransactionCreateArgs { actions, memo },
    };

    Ok(Instruction::new_with_bytes(
        *program_id,
        &data.data()?,
        accounts,
    ))
}

/// Build a TransactionMessage for a parent multisig to CREATE a child's vault transaction
/// (using an already-built `TransactionMessage`) and its proposal in a single parent transaction.
/// The parent vault PDA acts as creator; the provided rent payer funds the account creations.
/// Caller must ensure the parent vault is a member of the child multisig with Initiate+Vote.
pub fn create_child_create_vault_transaction_and_proposal_message(
    child_multisig: Pubkey,
    child_tx_index: u64,
    parent_member_pubkey: Pubkey,
    rent_payer_pubkey: Pubkey,
    transaction_message: TransactionMessage,
) -> eyre::Result<TransactionMessage> {
    use crate::squads::{get_proposal_pda, get_transaction_pda, SQUADS_MULTISIG_PROGRAM_ID};

    let (transaction_pda, _) = get_transaction_pda(
        &child_multisig,
        child_tx_index,
        Some(&SQUADS_MULTISIG_PROGRAM_ID),
    );
    let (proposal_pda, _) = get_proposal_pda(
        &child_multisig,
        child_tx_index,
        Some(&SQUADS_MULTISIG_PROGRAM_ID),
    );

    let create_transaction_instruction = create_vault_transaction_create_instruction(
        &SQUADS_MULTISIG_PROGRAM_ID,
        &child_multisig,
        &transaction_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        0,
        transaction_message,
    )?;
    let create_proposal_instruction = create_proposal_create_instruction(
        &SQUADS_MULTISIG_PROGRAM_ID,
        &child_multisig,
        &proposal_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        child_tx_index,
    )?;

    build_squads_transaction_message(
        &[create_transaction_instruction, create_proposal_instruction],
        &parent_member_pubkey,
    )
}

/// Build a TransactionMessage for feature gate activation or revocation.
/// This creates a vault transaction that calls the feature gate program to activate/revoke a feature.
pub fn create_feature_gate_transaction_message(
    feature_id: Pubkey,
    _vault_pda: Pubkey,
    operation: crate::commands::TransactionKind,
) -> eyre::Result<TransactionMessage> {
    use crate::feature_gate_program;

    // Build the feature gate instruction based on operation type
    // Note: feature_id IS the vault PDA, which is unallocated but exists.
    // So we use activate_feature_funded which allocates and assigns it without transfer.
    let instructions = match operation {
        crate::commands::TransactionKind::Activate => {
            feature_gate_program::activate_feature_funded(&feature_id)
        }
        crate::commands::TransactionKind::Revoke => {
            vec![feature_gate_program::revoke_pending_activation(&feature_id)]
        }
        crate::commands::TransactionKind::Rekey => {
            return Err(eyre::eyre!("Rekey is not a feature gate operation"));
        }
    };

    // Use the centralized helper to build the TransactionMessage
    // The feature_id is the signer (vault PDA), so we pass it as the payer
    build_squads_transaction_message(&instructions, &feature_id)
}

/// Create an execute message for any Squads multisig proposal at `proposal_index`.
pub fn create_execute_transaction_message(
    program_id: &Pubkey,
    multisig_address: &Pubkey,
    member_pubkey: &Pubkey,
    fee_payer_pubkey: &Pubkey,
    proposal_index: u64,
    rpc_client: &RpcClient,
    recent_blockhash: Hash,
) -> eyre::Result<Message> {
    use crate::squads::{
        get_transaction_pda, get_vault_pda, MultisigExecuteTransactionAccounts, VaultTransaction,
    };

    let (proposal_pda, _proposal_bump) =
        get_proposal_pda(multisig_address, proposal_index, Some(program_id));
    let (transaction_pda, _transaction_bump) =
        get_transaction_pda(multisig_address, proposal_index, Some(program_id));
    let _vault_pda = get_vault_pda(multisig_address, 0, Some(program_id));

    let transaction_account_data = rpc_client.get_account_data(&transaction_pda)?;
    let transaction_contents = VaultTransaction::try_from_slice(&transaction_account_data[8..])
        .map_err(|e| {
            eyre::eyre!(
                "Failed to deserialize vault transaction at {}: {}",
                transaction_pda,
                e
            )
        })?;
    let transaction_message = transaction_contents.message;

    let mut execution_account_metas = Vec::new();
    for (i, account_key) in transaction_message.account_keys.iter().enumerate() {
        // Do NOT preserve signer flags in the outer instruction. The Squads program
        // reads the stored TransactionMessage which has num_signers to know which accounts
        // to PDA-sign during CPI. Marking them as signers here causes message construction issues.
        let is_signer = false;

        let is_writable = transaction_message.is_static_writable_index(i);
        if is_writable {
            execution_account_metas.push(AccountMeta::new(*account_key, is_signer));
        } else {
            execution_account_metas.push(AccountMeta::new_readonly(*account_key, is_signer));
        }
    }

    // All accounts from the stored message are passed through to the Squads program,
    // including the parent multisig needed for vault PDA derivation during CPI.

    let account_keys = MultisigExecuteTransactionAccounts {
        multisig: *multisig_address,
        proposal: proposal_pda,
        transaction: transaction_pda,
        member: *member_pubkey,
    };

    let account_metas = account_keys.to_account_metas(execution_account_metas);

    let execute_instruction = Instruction::new_with_bytes(
        *program_id,
        &EXECUTE_TRANSACTION_DISCRIMINATOR,
        account_metas,
    );

    let message = Message::try_compile(
        fee_payer_pubkey,
        &[execute_instruction],
        &[],
        recent_blockhash,
    )?;

    Ok(message)
}

/// Create an execute message for a Squads config transaction at `transaction_index`.
/// Config transactions do not embed vault account metas, so the base accounts are sufficient.
pub fn create_execute_config_transaction_message(
    program_id: &Pubkey,
    multisig_address: &Pubkey,
    member_pubkey: &Pubkey,
    fee_payer_pubkey: &Pubkey,
    rent_payer: Option<Pubkey>,
    transaction_index: u64,
    recent_blockhash: Hash,
) -> eyre::Result<Message> {
    use crate::squads::{get_proposal_pda, get_transaction_pda};

    let (proposal_pda, _) = get_proposal_pda(multisig_address, transaction_index, Some(program_id));
    let (transaction_pda, _) =
        get_transaction_pda(multisig_address, transaction_index, Some(program_id));

    // Match SDK account layout/order: multisig (w), member (ro, signer), proposal (w),
    // transaction (ro), rent_payer?, system_program?.
    let mut account_metas = vec![
        AccountMeta::new(*multisig_address, false),
        AccountMeta::new_readonly(*member_pubkey, true),
        AccountMeta::new(proposal_pda, false),
        AccountMeta::new_readonly(transaction_pda, false),
    ];

    // Rent payer and system program follow SDK defaults.
    if let Some(rent) = rent_payer {
        account_metas.push(AccountMeta::new(rent, true));
    } else {
        account_metas.push(AccountMeta::new_readonly(*program_id, false));
    }

    account_metas.push(AccountMeta::new_readonly(
        solana_system_interface::program::ID,
        false,
    ));

    let execute_instruction = Instruction::new_with_bytes(
        *program_id,
        &CONFIG_TRANSACTION_EXECUTE_DISCRIMINATOR,
        account_metas,
    );

    let message = Message::try_compile(
        fee_payer_pubkey,
        &[execute_instruction],
        &[],
        recent_blockhash,
    )?;

    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::squads::CompiledInstruction;
    use borsh::BorshDeserialize;

    fn create_test_transaction_message() -> TransactionMessage {
        use crate::feature_gate_program::activate_feature_funded;

        // Create feature activation instructions for a test feature (account already funded)
        let feature_id = Pubkey::new_unique();
        let instructions = activate_feature_funded(&feature_id);

        // Build account keys list for the message
        let mut account_keys = vec![
            feature_id,                           // 0: Feature account (signer, writable)
            solana_system_interface::program::ID, // 1: System program
            crate::feature_gate_program::FEATURE_GATE_PROGRAM_ID, // 2: Feature gate program
        ];

        // Compile instructions into MultisigCompiledInstructions
        let mut compiled_instructions = Vec::new();

        for instruction in instructions {
            // Find program_id index in account_keys
            let program_id_index = account_keys
                .iter()
                .position(|key| *key == instruction.program_id)
                .unwrap_or_else(|| {
                    account_keys.push(instruction.program_id);
                    account_keys.len() - 1
                }) as u8;

            // Map account pubkeys to indices
            let account_indexes: Vec<u8> = instruction
                .accounts
                .iter()
                .map(|account_meta| {
                    account_keys
                        .iter()
                        .position(|key| *key == account_meta.pubkey)
                        .unwrap_or_else(|| {
                            account_keys.push(account_meta.pubkey);
                            account_keys.len() - 1
                        }) as u8
                })
                .collect();

            compiled_instructions.push(CompiledInstruction {
                program_id_index,
                account_indexes: SmallVec::from(account_indexes),
                data: SmallVec::from(instruction.data),
            });
        }

        TransactionMessage {
            num_signers: 1,              // feature_id is the signer
            num_writable_signers: 1,     // feature_id is writable signer
            num_writable_non_signers: 0, // no writable non-signers
            account_keys: SmallVec::from(account_keys),
            instructions: SmallVec::from(compiled_instructions),
            address_table_lookups: SmallVec::from(vec![]),
        }
    }

    #[test]
    fn test_create_transaction_data_serialization() {
        let transaction_message = create_test_transaction_message();

        let transaction_message_bytes = borsh::to_vec(&transaction_message).unwrap();
        let create_transaction_data = VaultTransactionCreateArgsData {
            args: VaultTransactionCreateArgs {
                vault_index: 0,
                ephemeral_signers: 0,
                transaction_message: transaction_message_bytes,
                memo: None,
            },
        };

        // Serialize the data
        let serialized_data = create_transaction_data.data().unwrap();

        // Check that it starts with the correct discriminator
        assert_eq!(
            &serialized_data[0..8],
            crate::squads::CREATE_TRANSACTION_DISCRIMINATOR
        );

        // Test deserialization of the args portion
        let args_data = &serialized_data[8..];
        let deserialized_args = VaultTransactionCreateArgs::try_from_slice(args_data).unwrap();

        assert_eq!(deserialized_args.vault_index, 0);
        assert_eq!(deserialized_args.ephemeral_signers, 0);
        assert_eq!(deserialized_args.memo, None);

        // Deserialize the transaction message bytes and verify
        let deserialized_transaction_message =
            TransactionMessage::try_from_slice(&deserialized_args.transaction_message).unwrap();
        assert_eq!(
            deserialized_transaction_message.num_signers,
            transaction_message.num_signers
        );
        assert_eq!(
            deserialized_transaction_message.account_keys.len(),
            transaction_message.account_keys.len()
        );
    }

    #[test]
    fn test_create_proposal_data_serialization() {
        let create_proposal_data = MultisigCreateProposalData {
            args: MultisigCreateProposalArgs {
                transaction_index: 1,
                is_draft: false,
            },
        };

        // Serialize the data
        let serialized_data = create_proposal_data.data().unwrap();

        // Check that it starts with the correct discriminator
        assert_eq!(
            &serialized_data[0..8],
            crate::squads::CREATE_PROPOSAL_DISCRIMINATOR
        );

        // Test deserialization of the args portion
        let args_data = &serialized_data[8..];
        let deserialized_args = MultisigCreateProposalArgs::try_from_slice(args_data).unwrap();

        assert_eq!(deserialized_args.transaction_index, 1);
        assert_eq!(deserialized_args.is_draft, false);
    }

    #[test]
    fn test_vault_transaction_message_serialization() {
        let transaction_message = create_test_transaction_message();

        // Test serialization and deserialization
        let serialized = borsh::to_vec(&transaction_message).unwrap();
        let deserialized = TransactionMessage::try_from_slice(&serialized).unwrap();

        assert_eq!(deserialized.num_signers, transaction_message.num_signers);
        assert_eq!(
            deserialized.num_writable_signers,
            transaction_message.num_writable_signers
        );
        assert_eq!(
            deserialized.num_writable_non_signers,
            transaction_message.num_writable_non_signers
        );
        assert_eq!(deserialized.account_keys, transaction_message.account_keys);
        assert_eq!(
            deserialized.instructions.len(),
            transaction_message.instructions.len()
        );
        assert_eq!(
            deserialized.instructions[0].program_id_index,
            transaction_message.instructions[0].program_id_index
        );
        assert_eq!(
            deserialized.instructions[0].account_indexes,
            transaction_message.instructions[0].account_indexes
        );
        assert_eq!(
            deserialized.instructions[0].data,
            transaction_message.instructions[0].data
        );
    }

    #[test]
    fn test_pda_derivation() {
        let multisig_address = Pubkey::new_unique(); // Generate random key
        let transaction_index = 1u64;

        // Test transaction PDA derivation
        let (transaction_pda, _) = get_transaction_pda(&multisig_address, transaction_index, None);

        // Test proposal PDA derivation
        let (proposal_pda, _) = get_proposal_pda(&multisig_address, transaction_index, None);

        // PDAs should be different
        assert_ne!(transaction_pda, proposal_pda);

        // Same inputs should produce same PDAs
        let (transaction_pda2, _) = get_transaction_pda(&multisig_address, transaction_index, None);
        let (proposal_pda2, _) = get_proposal_pda(&multisig_address, transaction_index, None);
        assert_eq!(transaction_pda, transaction_pda2);
        assert_eq!(proposal_pda, proposal_pda2);
    }

    #[test]
    fn test_account_metas_generation() {
        let multisig = Pubkey::new_unique();
        let transaction = Pubkey::new_unique();
        let proposal = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let rent_payer = Pubkey::new_unique();

        // Test MultisigCreateTransaction account metas
        let create_transaction_accounts = MultisigCreateTransaction {
            multisig,
            transaction,
            creator,
            rent_payer,
            system_program: solana_system_interface::program::ID,
        };

        let tx_metas = create_transaction_accounts.to_account_metas();
        assert_eq!(tx_metas.len(), 5);
        assert_eq!(tx_metas[0].pubkey, multisig);
        assert_eq!(tx_metas[1].pubkey, transaction);
        assert_eq!(tx_metas[2].pubkey, creator);
        assert_eq!(tx_metas[3].pubkey, rent_payer);
        assert_eq!(tx_metas[4].pubkey, solana_system_interface::program::ID);

        // Test MultisigCreateProposal account metas
        let create_proposal_accounts = MultisigCreateProposalAccounts {
            multisig,
            proposal,
            creator,
            rent_payer,
            system_program: solana_system_interface::program::ID,
        };

        let proposal_metas = create_proposal_accounts.to_account_metas();
        assert_eq!(proposal_metas.len(), 5);
        assert_eq!(proposal_metas[0].pubkey, multisig);
        assert_eq!(proposal_metas[1].pubkey, proposal);
        assert_eq!(proposal_metas[2].pubkey, creator);
        assert_eq!(proposal_metas[3].pubkey, rent_payer);
        assert_eq!(
            proposal_metas[4].pubkey,
            solana_system_interface::program::ID
        );
    }

    #[test]
    fn test_build_squads_transaction_message_is_stable_and_upgrades_permissions() {
        let writable_signer_a = Pubkey::new_unique();
        let writable_signer_b = Pubkey::new_unique();
        let readonly_signer = Pubkey::new_unique();
        let writable_non_signer = Pubkey::new_unique();
        let readonly_non_signer = Pubkey::new_unique();
        let program_a = Pubkey::new_unique();
        let program_b = Pubkey::new_unique();

        let instructions = vec![
            Instruction {
                program_id: program_a,
                accounts: vec![
                    AccountMeta::new_readonly(writable_signer_a, true),
                    AccountMeta::new(writable_non_signer, false),
                    AccountMeta::new_readonly(readonly_non_signer, false),
                ],
                data: vec![1],
            },
            Instruction {
                program_id: program_b,
                accounts: vec![
                    AccountMeta::new(writable_signer_b, true),
                    AccountMeta::new(writable_signer_a, true),
                    AccountMeta::new_readonly(readonly_signer, true),
                    AccountMeta::new_readonly(writable_non_signer, false),
                ],
                data: vec![2],
            },
        ];

        let message = build_squads_transaction_message(&instructions, &writable_signer_a).unwrap();

        assert_eq!(message.num_signers, 3);
        assert_eq!(message.num_writable_signers, 2);
        assert_eq!(message.num_writable_non_signers, 1);
        let account_keys: Vec<_> = message.account_keys.iter().copied().collect();
        assert_eq!(
            account_keys,
            vec![
                writable_signer_a,
                writable_signer_b,
                readonly_signer,
                writable_non_signer,
                readonly_non_signer,
                program_a,
                program_b,
            ]
        );
        assert_eq!(message.instructions[0].program_id_index, 5);
        let first_instruction_accounts: Vec<_> = message.instructions[0]
            .account_indexes
            .iter()
            .copied()
            .collect();
        assert_eq!(first_instruction_accounts, vec![0, 3, 4]);
        assert_eq!(message.instructions[1].program_id_index, 6);
        let second_instruction_accounts: Vec<_> = message.instructions[1]
            .account_indexes
            .iter()
            .copied()
            .collect();
        assert_eq!(second_instruction_accounts, vec![1, 0, 2, 3]);
    }

    #[test]
    fn test_create_transaction_and_proposal_message() {
        let multisig_address = Pubkey::new_unique();
        let fee_payer_pubkey = Pubkey::new_unique();
        let contributor_pubkey = Pubkey::new_unique();
        let recent_blockhash = Hash::default(); // Use default hash for testing

        let transaction_message = create_test_transaction_message();
        let transaction_index = 1u64;
        let vault_index = 0u8;
        let priority_fee = Some(5000u32);

        // Test message creation
        let result = create_transaction_and_proposal_message(
            None, // Use default program ID
            &fee_payer_pubkey,
            &contributor_pubkey,
            &multisig_address,
            transaction_index,
            vault_index,
            transaction_message,
            priority_fee,
            Some(200000u32), // compute_unit_limit
            recent_blockhash,
        );

        assert!(result.is_ok());
        let (message, transaction_pda, proposal_pda) = result.unwrap();

        // Verify PDAs are derived correctly
        let expected_transaction_pda =
            get_transaction_pda(&multisig_address, transaction_index, None).0;
        let expected_proposal_pda = get_proposal_pda(&multisig_address, transaction_index, None).0;
        assert_eq!(transaction_pda, expected_transaction_pda);
        assert_eq!(proposal_pda, expected_proposal_pda);

        // Verify message has the right number of instructions
        // Should have 4: priority fee + compute unit limit + create transaction + create proposal
        assert_eq!(message.instructions.len(), 4);

        // Verify the fee payer is set correctly
        assert_eq!(message.account_keys[0], fee_payer_pubkey);

        // Verify PDAs are not the same
        assert_ne!(transaction_pda, proposal_pda);
    }

    #[test]
    fn test_create_transaction_and_proposal_message_no_priority_fee() {
        let multisig_address = Pubkey::new_unique();
        let fee_payer_pubkey = Pubkey::new_unique();
        let contributor_pubkey = Pubkey::new_unique();
        let recent_blockhash = Hash::default(); // Use default hash for testing

        let transaction_message = create_test_transaction_message();
        let transaction_index = 1u64;
        let vault_index = 0u8;

        // Test message creation without priority fee
        let result = create_transaction_and_proposal_message(
            None, // Use default program ID
            &fee_payer_pubkey,
            &contributor_pubkey,
            &multisig_address,
            transaction_index,
            vault_index,
            transaction_message,
            None, // No priority fee
            None, // No compute unit limit
            recent_blockhash,
        );

        assert!(result.is_ok());
        let (message, _transaction_pda, _proposal_pda) = result.unwrap();

        // Should have 2 instructions: create transaction + create proposal (no priority fee)
        assert_eq!(message.instructions.len(), 2);
    }

    #[test]
    fn test_debug_serialization() {
        let transaction_message = create_test_transaction_message();

        println!("Transaction message created with:");
        println!("  num_signers: {}", transaction_message.num_signers);
        println!(
            "  num_writable_signers: {}",
            transaction_message.num_writable_signers
        );
        println!(
            "  num_writable_non_signers: {}",
            transaction_message.num_writable_non_signers
        );
        println!(
            "  account_keys.len(): {}",
            transaction_message.account_keys.len()
        );
        println!(
            "  instructions.len(): {}",
            transaction_message.instructions.len()
        );

        // Try to serialize just the transaction message
        let serialized = borsh::to_vec(&transaction_message).unwrap();
        println!(
            "  serialized transaction_message length: {}",
            serialized.len()
        );

        // Show detailed hex breakdown
        println!("  Detailed serialization breakdown:");
        println!("    num_signers (u8): {:02x}", serialized[0]);
        println!("    num_writable_signers (u8): {:02x}", serialized[1]);
        println!("    num_writable_non_signers (u8): {:02x}", serialized[2]);

        // Check account_keys serialization - should be length as u8 then pubkeys
        println!("    account_keys length byte: {:02x}", serialized[3]);

        // If it shows more than 1 byte for length, there's the issue
        println!(
            "    bytes 4-7: {:02x} {:02x} {:02x} {:02x}",
            serialized[4], serialized[5], serialized[6], serialized[7]
        );

        // Create VaultTransactionCreateArgs and see its serialization
        let transaction_message_bytes = borsh::to_vec(&transaction_message).unwrap();
        let vault_args = VaultTransactionCreateArgs {
            vault_index: 0,
            ephemeral_signers: 0,
            transaction_message: transaction_message_bytes.clone(),
            memo: None,
        };

        let vault_args_serialized = borsh::to_vec(&vault_args).unwrap();
        println!(
            "  vault_args serialized length: {}",
            vault_args_serialized.len()
        );
        println!("  vault_args hex breakdown:");
        println!("    vault_index: {:02x}", vault_args_serialized[0]);
        println!("    ephemeral_signers: {:02x}", vault_args_serialized[1]);

        // Next should be the Vec<u8> length (u32) then the transaction_message bytes
        let tm_vec_len = u32::from_le_bytes([
            vault_args_serialized[2],
            vault_args_serialized[3],
            vault_args_serialized[4],
            vault_args_serialized[5],
        ]);
        println!(
            "    transaction_message Vec<u8> length: {} bytes",
            tm_vec_len
        );

        // The actual transaction message bytes start at offset 6, then after memo option
        let tm_offset = 6;

        // memo is Option<String> which serializes as 1 byte (0 for None, 1 for Some) + content
        let memo_len_byte = vault_args_serialized[tm_offset + tm_vec_len as usize];
        println!(
            "    memo option byte: {:02x} (0=None, 1=Some)",
            memo_len_byte
        );

        // Transaction message data starts after the memo
        let actual_tm_offset = tm_offset;
        if vault_args_serialized.len() > actual_tm_offset + 5 {
            println!("    tm bytes start at offset {}", actual_tm_offset);
            println!(
                "    tm.num_signers: {:02x}",
                vault_args_serialized[actual_tm_offset]
            );
            println!(
                "    tm.num_writable_signers: {:02x}",
                vault_args_serialized[actual_tm_offset + 1]
            );
            println!(
                "    tm.num_writable_non_signers: {:02x}",
                vault_args_serialized[actual_tm_offset + 2]
            );
            println!(
                "    tm.account_keys length: {:02x}",
                vault_args_serialized[actual_tm_offset + 3]
            );
        }

        // Create the full data structure
        let create_transaction_data = VaultTransactionCreateArgsData { args: vault_args };

        let full_data = create_transaction_data.data().unwrap();
        println!("  full data length: {}", full_data.len());

        // Convert to hex string like the blockchain data
        let hex_string = full_data
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        println!("  full hex: {}", hex_string);
    }

    #[test]
    fn test_feature_activation_instructions_compilation() {
        let transaction_message = create_test_transaction_message();

        // Verify we have 2 compiled instructions (allocate, assign) for funded feature activation
        assert_eq!(transaction_message.instructions.len(), 2);

        // Verify account structure (3 accounts: feature, system program, feature gate program)
        assert!(transaction_message.account_keys.len() >= 3);

        // Verify signer counts (feature account is the only signer and is writable)
        assert_eq!(transaction_message.num_signers, 1);
        assert_eq!(transaction_message.num_writable_signers, 1);
        assert_eq!(transaction_message.num_writable_non_signers, 0);

        // First account is the feature account (writable signer)
        // System program and Feature Gate program should be in the account list
        let has_system_program = transaction_message
            .account_keys
            .contains(&solana_system_interface::program::ID);
        let has_feature_gate_program = transaction_message
            .account_keys
            .contains(&crate::feature_gate_program::FEATURE_GATE_PROGRAM_ID);

        assert!(
            has_system_program,
            "Transaction should include system program"
        );
        assert!(
            has_feature_gate_program,
            "Transaction should include feature gate program"
        );

        // Verify all instructions have valid program_id_index and account_indexes
        for (i, instruction) in transaction_message.instructions.iter().enumerate() {
            assert!(
                (instruction.program_id_index as usize) < transaction_message.account_keys.len(),
                "Instruction {} has invalid program_id_index",
                i
            );

            for (j, &account_index) in instruction.account_indexes.iter().enumerate() {
                assert!(
                    (account_index as usize) < transaction_message.account_keys.len(),
                    "Instruction {} account {} has invalid index",
                    i,
                    j
                );
            }

            // Each instruction should have some data
            assert!(
                !instruction.data.is_empty(),
                "Instruction {} should have data",
                i
            );
        }
    }
}
