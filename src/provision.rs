use crate::constants::*;
use crate::squads::{
    deserialize_squads_account, get_multisig_pda, get_program_config_pda, get_proposal_pda,
    get_transaction_pda, get_vault_pda, CompiledInstruction, InstructionData, Member,
    MessageAddressTableLookup, MultisigCreateArgsV2, MultisigCreateProposalAccounts,
    MultisigCreateProposalArgs, MultisigCreateProposalData, MultisigCreateTransaction,
    MultisigCreateV2Accounts, MultisigCreateV2Data, MultisigExecuteTransactionAccounts,
    MultisigExecuteTransactionArgs, MultisigVoteOnProposalAccounts, MultisigVoteOnProposalArgs,
    ProgramConfig, SmallVec, TransactionMessage, VaultTransactionCreateArgs,
    VaultTransactionCreateArgsData, CONFIG_TRANSACTION_EXECUTE_DISCRIMINATOR,
    EXECUTE_TRANSACTION_DISCRIMINATOR, MULTISIG_ACCOUNT_DISCRIMINATOR,
    PROGRAM_CONFIG_ACCOUNT_DISCRIMINATOR, PROPOSAL_ACCOUNT_DISCRIMINATOR,
    SQUADS_MULTISIG_PROGRAM_ID, VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
};

use crate::utils::{decode_permissions, get_network_display};
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
use solana_message::{AddressLookupTableAccount, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::time::Duration;

/// Creates an RPC client with consistent commitment configuration
pub fn create_rpc_client(url: &str) -> RpcClient {
    RpcClient::new_with_commitment(url, CommitmentConfig::confirmed())
}

/// Fetch a multisig account and validate owner + Anchor discriminator before deserializing.
/// Use for any multisig address that originates from user input.
pub fn fetch_squads_multisig(
    rpc_client: &RpcClient,
    address: &Pubkey,
    account_kind: &str,
) -> eyre::Result<crate::squads::Multisig> {
    let acc = rpc_client
        .get_account(address)
        .map_err(|e| eyre!("Failed to fetch {} {}: {}", account_kind, address, e))?;
    if acc.owner != SQUADS_MULTISIG_PROGRAM_ID {
        return Err(eyre!(
            "{} {} is not owned by the Squads program (owner: {})",
            account_kind,
            address,
            acc.owner
        ));
    }
    deserialize_squads_account(&acc.data, MULTISIG_ACCOUNT_DISCRIMINATOR, account_kind)
}

/// Compile `instructions` into a Squads `TransactionMessage` using Solana's official v0
/// message compiler, then translate the result into the Squads wire format.
///
/// `payer` becomes `account_keys[0]` — the required fee-payer/signer — matching Solana's
/// account-ordering invariant. For Squads inner messages this is the vault PDA that signs
/// via CPI. `address_lookup_table_accounts` lets callers pull accounts from on-chain lookup
/// tables; pass `&[]` to keep every account static.
///
/// The Squads inner message carries no blockhash of its own (it lives inside the outer
/// transaction), so a placeholder blockhash is supplied to the compiler and discarded.
pub fn build_squads_transaction_message(
    payer: &Pubkey,
    instructions: &[Instruction],
    address_lookup_table_accounts: &[AddressLookupTableAccount],
) -> eyre::Result<TransactionMessage> {
    let compiled = Message::try_compile(
        payer,
        instructions,
        address_lookup_table_accounts,
        Hash::default(),
    )?;

    let num_signers = compiled.header.num_required_signatures;
    let num_writable_signers = num_signers
        .checked_sub(compiled.header.num_readonly_signed_accounts)
        .ok_or_else(|| eyre!("compiled message has more readonly signers than signers"))?;
    let num_writable_non_signers = (compiled.account_keys.len() as u8)
        .checked_sub(num_signers)
        .and_then(|non_signers| {
            non_signers.checked_sub(compiled.header.num_readonly_unsigned_accounts)
        })
        .ok_or_else(|| eyre!("compiled message header counts exceed account_keys length"))?;

    let instructions: Vec<CompiledInstruction> = compiled
        .instructions
        .into_iter()
        .map(|ix| CompiledInstruction {
            program_id_index: ix.program_id_index,
            account_indexes: SmallVec::from(ix.accounts),
            data: SmallVec::from(ix.data),
        })
        .collect();

    let address_table_lookups: Vec<MessageAddressTableLookup> = compiled
        .address_table_lookups
        .into_iter()
        .map(|lookup| MessageAddressTableLookup {
            account_key: lookup.account_key,
            writable_indexes: SmallVec::from(lookup.writable_indexes),
            readonly_indexes: SmallVec::from(lookup.readonly_indexes),
        })
        .collect();

    Ok(TransactionMessage {
        num_signers,
        num_writable_signers,
        num_writable_non_signers,
        account_keys: SmallVec::from(compiled.account_keys),
        instructions: SmallVec::from(instructions),
        address_table_lookups: SmallVec::from(address_table_lookups),
    })
}

/// Returns true for transient node/network errors worth retrying — not deterministic
/// failures like a preflight/program error.
fn is_transient_rpc_error(err: &solana_client::client_error::ClientError) -> bool {
    match &err.kind {
        ClientErrorKind::RpcError(RpcError::RpcResponseError { code, .. }) => {
            // -32005 node unhealthy, -32004 request timed out, -32603 internal error.
            *code == -32005 || *code == -32004 || *code == -32603
        }
        ClientErrorKind::Io(_) | ClientErrorKind::Reqwest(_) => true,
        _ => false,
    }
}

pub fn send_and_confirm_transaction(
    transaction: &VersionedTransaction,
    rpc_client: &RpcClient,
) -> eyre::Result<String> {
    let config = RpcSendTransactionConfig {
        skip_preflight: false,
        preflight_commitment: Some(rpc_client.commitment().commitment),
        // Leave max_retries unset so the RPC node rebroadcasts until the blockhash
        // expires; we poll for confirmation rather than resending ourselves.
        ..Default::default()
    };

    // Send, retrying only transient node/network errors. A preflight failure is a
    // deterministic program error, so surface its logs and fail immediately.
    let mut signature = None;
    let mut last_send_err = None;
    for attempt in 0..MAX_TX_RETRIES {
        match rpc_client.send_transaction_with_config(transaction, config) {
            Ok(sig) => {
                signature = Some(sig);
                break;
            }
            Err(err) => {
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
                if !is_transient_rpc_error(&err) {
                    return Err(eyre!("Failed to send transaction: {}", err));
                }
                println!(
                    "Transient send error (attempt {}/{}): {}",
                    attempt + 1,
                    MAX_TX_RETRIES,
                    err.to_string().bright_yellow()
                );
                last_send_err = Some(err);
                if attempt + 1 < MAX_TX_RETRIES {
                    let delay = BASE_RETRY_DELAY_MS * 2_u64.pow(attempt as u32);
                    std::thread::sleep(Duration::from_millis(delay));
                }
            }
        }
    }

    let signature = signature.ok_or_else(|| {
        eyre!(
            "Failed to send transaction after {} attempts: {}",
            MAX_TX_RETRIES,
            last_send_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        )
    })?;

    // Poll for confirmation until the timeout, tolerating transient polling errors.
    let confirmation_start = std::time::Instant::now();
    loop {
        match rpc_client.get_signature_status(&signature) {
            Ok(Some(Ok(()))) => return Ok(signature.to_string()),
            Ok(Some(Err(e))) => {
                return Err(eyre!("Transaction {} failed on-chain: {}", signature, e))
            }
            Ok(None) => {}
            Err(err) if !is_transient_rpc_error(&err) => {
                return Err(eyre!(
                    "Failed to confirm transaction {}: {}",
                    signature,
                    err
                ));
            }
            Err(_) => {}
        }

        if confirmation_start.elapsed().as_millis() as u64 > CONFIRMATION_TIMEOUT_MS {
            return Err(eyre!(
                "Transaction {} not confirmed within {}ms",
                signature,
                CONFIRMATION_TIMEOUT_MS
            ));
        }
        std::thread::sleep(Duration::from_millis(CONFIRMATION_POLL_INTERVAL_MS));
    }
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
pub fn create_multisig(
    rpc_url: String,
    fee_payer_keypair: &dyn Signer,
    create_key: &Keypair,
    members: Vec<Member>,
    threshold: u16,
    priority_fee_lamports: Option<u64>,
) -> eyre::Result<(Pubkey, String)> {
    let multisig_address = crate::squads::get_multisig_pda(&create_key.pubkey()).0;
    let vault_address = get_vault_pda(&multisig_address, 0).0;

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
        SQUADS_MULTISIG_PROGRAM_ID.to_string().bright_white()
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
    println!();
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

    let multisig_key = get_multisig_pda(&create_key.pubkey());

    let program_config_pda = get_program_config_pda();

    let program_config = rpc_client
        .get_account(&program_config_pda.0)
        .map_err(|e| eyre::eyre!("Failed to fetch program config account: {}", e))?;

    let treasury = deserialize_squads_account::<ProgramConfig>(
        &program_config.data,
        PROGRAM_CONFIG_ACCOUNT_DISCRIMINATOR,
        "program config",
    )?
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
                program_id: SQUADS_MULTISIG_PROGRAM_ID,
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

// Builds a vault-transaction-plus-proposal message; the wide signature mirrors the
// distinct on-chain inputs (keys, indices, compute budget, blockhash) with no natural grouping.
#[allow(clippy::too_many_arguments)]
pub fn create_transaction_and_proposal_message(
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
    let (transaction_pda, _transaction_bump) =
        get_transaction_pda(multisig_address, transaction_index);
    let (proposal_pda, _proposal_bump) = get_proposal_pda(multisig_address, transaction_index);

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
        multisig_address,
        &transaction_pda,
        contributor_pubkey,
        fee_payer_pubkey,
        vault_index,
        transaction_message,
    )?);
    instructions.push(create_proposal_create_instruction(
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
        SQUADS_MULTISIG_PROGRAM_ID,
        &data.data()?,
        accounts.to_account_metas(),
    ))
}

pub(crate) fn create_proposal_create_instruction(
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
        SQUADS_MULTISIG_PROGRAM_ID,
        &data.data()?,
        accounts.to_account_metas(),
    ))
}

/// Create a vote (approve or reject) message for a proposal.
/// Pass `PROPOSAL_APPROVE_DISCRIMINATOR` for approval or `PROPOSAL_REJECT_DISCRIMINATOR` for rejection.
pub fn create_vote_proposal_message(
    multisig_address: &Pubkey,
    member_pubkey: &Pubkey,
    fee_payer_pubkey: &Pubkey,
    proposal_index: u64,
    recent_blockhash: Hash,
    discriminator: &[u8],
) -> eyre::Result<Message> {
    let (proposal_pda, _proposal_bump) = get_proposal_pda(multisig_address, proposal_index);

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
        SQUADS_MULTISIG_PROGRAM_ID,
        &instruction_data,
        account_keys.to_account_metas(),
    );

    let message =
        Message::try_compile(fee_payer_pubkey, &[vote_instruction], &[], recent_blockhash)?;

    Ok(message)
}

/// Fetch proposal approvals count, parent threshold, and proposal status for a given proposal index.
pub fn get_proposal_status_and_threshold(
    multisig_address: &Pubkey,
    proposal_index: u64,
    rpc_client: &RpcClient,
) -> eyre::Result<(usize, u16, crate::squads::ProposalStatus)> {
    use crate::squads::Proposal;

    // Multisig threshold
    let ms = fetch_squads_multisig(rpc_client, multisig_address, "multisig")?;

    // Proposal approved count and status
    let (proposal_pda, _) = get_proposal_pda(multisig_address, proposal_index);
    let prop_acc = rpc_client.get_account(&proposal_pda)?;
    let prop: Proposal =
        deserialize_squads_account(&prop_acc.data, PROPOSAL_ACCOUNT_DISCRIMINATOR, "proposal")?;

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
    let (proposal_pda, _) = get_proposal_pda(&child_multisig, child_tx_index);
    let (transaction_pda, _) = get_transaction_pda(&child_multisig, child_tx_index);

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
    build_squads_transaction_message(&parent_member_pubkey, &[ix], &[])
}

/// Build a TransactionMessage for a parent multisig to execute a child's config transaction.
/// Config transactions do not require extra execution metas beyond the base accounts.
pub fn create_child_execute_config_transaction_message(
    child_multisig: Pubkey,
    child_tx_index: u64,
    parent_member_pubkey: Pubkey,
) -> eyre::Result<TransactionMessage> {
    // Derive child's proposal and transaction PDAs
    let (proposal_pda, _) = get_proposal_pda(&child_multisig, child_tx_index);
    let (transaction_pda, _) = get_transaction_pda(&child_multisig, child_tx_index);

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
    build_squads_transaction_message(&parent_member_pubkey, &[ix], &[])
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
    let (transaction_pda, _) = get_transaction_pda(&child_multisig, child_tx_index);
    let (proposal_pda, _) = get_proposal_pda(&child_multisig, child_tx_index);

    let config_create_instruction = create_config_transaction_create_instruction(
        &child_multisig,
        &transaction_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        actions,
        memo,
    )?;
    let create_proposal_instruction = create_proposal_create_instruction(
        &child_multisig,
        &proposal_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        child_tx_index,
    )?;

    build_squads_transaction_message(
        &parent_member_pubkey,
        &[config_create_instruction, create_proposal_instruction],
        &[],
    )
}

pub(crate) fn create_config_transaction_create_instruction(
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
        SQUADS_MULTISIG_PROGRAM_ID,
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
    use crate::squads::{get_proposal_pda, get_transaction_pda};

    let (transaction_pda, _) = get_transaction_pda(&child_multisig, child_tx_index);
    let (proposal_pda, _) = get_proposal_pda(&child_multisig, child_tx_index);

    let create_transaction_instruction = create_vault_transaction_create_instruction(
        &child_multisig,
        &transaction_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        0,
        transaction_message,
    )?;
    let create_proposal_instruction = create_proposal_create_instruction(
        &child_multisig,
        &proposal_pda,
        &parent_member_pubkey,
        &rent_payer_pubkey,
        child_tx_index,
    )?;

    build_squads_transaction_message(
        &parent_member_pubkey,
        &[create_transaction_instruction, create_proposal_instruction],
        &[],
    )
}

/// Build a TransactionMessage for feature gate activation or revocation.
/// This creates a vault transaction that calls the feature gate program to activate/revoke a feature.
pub fn create_feature_gate_transaction_message(
    feature_id: Pubkey,
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
    build_squads_transaction_message(&feature_id, &instructions, &[])
}

/// Create an execute message for any Squads multisig proposal at `proposal_index`.
pub fn create_execute_transaction_message(
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

    let (proposal_pda, _proposal_bump) = get_proposal_pda(multisig_address, proposal_index);
    let (transaction_pda, _transaction_bump) =
        get_transaction_pda(multisig_address, proposal_index);
    let _vault_pda = get_vault_pda(multisig_address, 0);

    let transaction_account_data = rpc_client.get_account_data(&transaction_pda)?;
    let transaction_contents: VaultTransaction = deserialize_squads_account(
        &transaction_account_data,
        VAULT_TRANSACTION_ACCOUNT_DISCRIMINATOR,
        "vault transaction",
    )
    .map_err(|e| eyre::eyre!("at {}: {}", transaction_pda, e))?;
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
        SQUADS_MULTISIG_PROGRAM_ID,
        EXECUTE_TRANSACTION_DISCRIMINATOR,
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
    multisig_address: &Pubkey,
    member_pubkey: &Pubkey,
    fee_payer_pubkey: &Pubkey,
    rent_payer: Option<Pubkey>,
    transaction_index: u64,
    recent_blockhash: Hash,
) -> eyre::Result<Message> {
    use crate::squads::{get_proposal_pda, get_transaction_pda};

    let (proposal_pda, _) = get_proposal_pda(multisig_address, transaction_index);
    let (transaction_pda, _) = get_transaction_pda(multisig_address, transaction_index);

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
        account_metas.push(AccountMeta::new_readonly(SQUADS_MULTISIG_PROGRAM_ID, false));
    }

    account_metas.push(AccountMeta::new_readonly(
        solana_system_interface::program::ID,
        false,
    ));

    let execute_instruction = Instruction::new_with_bytes(
        SQUADS_MULTISIG_PROGRAM_ID,
        CONFIG_TRANSACTION_EXECUTE_DISCRIMINATOR,
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
        assert!(!deserialized_args.is_draft);
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
        let (transaction_pda, _) = get_transaction_pda(&multisig_address, transaction_index);

        // Test proposal PDA derivation
        let (proposal_pda, _) = get_proposal_pda(&multisig_address, transaction_index);

        // PDAs should be different
        assert_ne!(transaction_pda, proposal_pda);

        // Same inputs should produce same PDAs
        let (transaction_pda2, _) = get_transaction_pda(&multisig_address, transaction_index);
        let (proposal_pda2, _) = get_proposal_pda(&multisig_address, transaction_index);
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

        let message =
            build_squads_transaction_message(&writable_signer_a, &instructions, &[]).unwrap();

        let account_keys: Vec<Pubkey> = message.account_keys.iter().copied().collect();

        // The payer is the required fee-payer/signer and must come first.
        assert_eq!(account_keys[0], writable_signer_a);

        // Header counts reflect the four account classes.
        assert_eq!(message.num_signers, 3);
        assert_eq!(message.num_writable_signers, 2);
        assert_eq!(message.num_writable_non_signers, 1);

        // Every account appears exactly once (writable_signer_a and writable_non_signer
        // are referenced by both instructions but deduplicated).
        let mut unique = account_keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), account_keys.len());
        assert_eq!(account_keys.len(), 7);

        // Each instruction's compiled indices resolve back to the original program and accounts.
        let resolve = |index: u8| account_keys[index as usize];
        for (compiled, original) in message.instructions.iter().zip(instructions.iter()) {
            assert_eq!(resolve(compiled.program_id_index), original.program_id);
            let resolved: Vec<Pubkey> = compiled
                .account_indexes
                .iter()
                .copied()
                .map(resolve)
                .collect();
            let expected: Vec<Pubkey> = original.accounts.iter().map(|meta| meta.pubkey).collect();
            assert_eq!(resolved, expected);
        }

        // Permissions are upgraded across instructions: writable_signer_a is a readonly
        // signer in instruction 0 but writable in instruction 1, so it lands among the
        // writable signers (the first `num_writable_signers` keys).
        let writable_signer_slice = &account_keys[..usize::from(message.num_writable_signers)];
        assert!(writable_signer_slice.contains(&writable_signer_a));
        assert!(writable_signer_slice.contains(&writable_signer_b));
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
        let expected_transaction_pda = get_transaction_pda(&multisig_address, transaction_index).0;
        let expected_proposal_pda = get_proposal_pda(&multisig_address, transaction_index).0;
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
