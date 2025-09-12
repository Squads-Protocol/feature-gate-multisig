use crate::constants::*;
use crate::squads::{
    get_multisig_pda, get_program_config_pda, get_proposal_pda, get_transaction_pda, get_vault_pda,
    Member, MultisigCreateArgsV2, MultisigCreateProposalAccounts, MultisigCreateProposalArgs,
    MultisigCreateProposalData, MultisigCreateTransaction, MultisigCreateV2Accounts,
    MultisigCreateV2Data, Permissions, ProgramConfig, TransactionMessage,
    VaultTransactionCreateArgs, VaultTransactionCreateArgsData,
};
use colored::Colorize;
use dialoguer::Confirm;
use eyre::eyre;
use indicatif::ProgressBar;
use solana_client::client_error::ClientErrorKind;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_client::rpc_request::{RpcError, RpcResponseErrorData};
use solana_client::rpc_response::RpcSimulateTransactionResult;
use solana_commitment_config::CommitmentConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::Instruction;
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

pub fn send_and_confirm_transaction(
    transaction: &VersionedTransaction,
    rpc_client: &RpcClient,
) -> eyre::Result<String> {
    const MAX_RETRIES: usize = MAX_TX_RETRIES;
    const BASE_DELAY_MS: u64 = BASE_RETRY_DELAY_MS;
    
    let mut last_error: Option<eyre::Report> = None;
    
    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = BASE_DELAY_MS * (2_u64.pow(attempt as u32 - 1));
            println!("Retrying transaction in {}ms... (attempt {}/{})", delay, attempt + 1, MAX_RETRIES);
            std::thread::sleep(Duration::from_millis(delay));
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
                        *code == -32001     // Generic server error
                    }
                    ClientErrorKind::Io(_) => true,  // Network issues
                    ClientErrorKind::Reqwest(_) => true,  // HTTP client issues  
                    _ => false
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
                
                println!("Retryable error occurred: {}", 
                    last_error.as_ref().unwrap().to_string().bright_yellow());
                continue;
            }
        };
        
        // Now wait for confirmation with exponential backoff polling
        let confirmation_start = std::time::Instant::now();
        let mut confirmation_poll_delay = CONFIRMATION_POLL_INTERVAL_MS;
        
        loop {
            if confirmation_start.elapsed().as_millis() as u64 > CONFIRMATION_TIMEOUT_MS {
                println!("Transaction confirmation timeout after {}ms", CONFIRMATION_TIMEOUT_MS);
                break; // Will retry sending
            }
            
            match rpc_client.get_signature_status(&signature) {
                Ok(Some(Ok(()))) => {
                    // Transaction confirmed successfully
                    println!(
                        "Transaction confirmed: {}\n\n",
                        signature.to_string().bright_green()
                    );
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
                                println!("Temporary confirmation error: {}", confirmation_err.to_string().bright_yellow());
                            } else {
                                // Non-retryable confirmation error, break and retry transaction
                                println!("Non-retryable confirmation error: {}", confirmation_err.to_string().bright_red());
                                break;
                            }
                        }
                        ClientErrorKind::Io(_) | ClientErrorKind::Reqwest(_) => {
                            // Network issues, continue polling
                            println!("Network error during confirmation: {}", confirmation_err.to_string().bright_yellow());
                        }
                        _ => {
                            // Unknown error, break and retry transaction
                            println!("Unknown confirmation error: {}", confirmation_err.to_string().bright_red());
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
        last_error = Some(eyre!("Transaction sent but confirmation failed or timed out"));
    }
    
    Err(eyre!(
        "Transaction failed after {} attempts: {}",
        MAX_RETRIES,
        last_error.map(|e| e.to_string()).unwrap_or_else(|| "Unknown error".to_string())
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
                    _ => false
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
        last_error.unwrap().to_string()
    ))
}
pub async fn create_multisig(
    rpc_url: String,
    program_id: Option<String>,
    contributor_keypair: &dyn Signer,
    create_key: &Keypair,
    config_authority: Option<Pubkey>,
    members: Vec<Member>,
    threshold: u16,
    rent_collector: Option<Pubkey>,
    priority_fee_lamports: Option<u64>,
) -> eyre::Result<(Pubkey, String)> {
    let program_id =
        program_id.unwrap_or_else(|| SQUADS_PROGRAM_ID_STR.to_string());
    let program_id = Pubkey::from_str(&program_id).expect("Invalid program ID");

    let transaction_creator = contributor_keypair.pubkey();

    println!();
    println!(
        "{}",
        "👀 You're about to create a multisig, please review the details:"
            .bright_yellow()
            .bold()
    );
    println!();
    println!("{}: {}", "RPC Cluster URL".cyan(), rpc_url.bright_white());
    println!(
        "{}: {}",
        "Program ID".cyan(),
        program_id.to_string().bright_white()
    );
    println!(
        "{}: {}",
        "Your Public Key".cyan(),
        transaction_creator.to_string().bright_white()
    );
    println!();
    println!("{}", "⚙️ Config Parameters".bright_yellow().bold());
    println!();
    println!(
        "{}: {}",
        "Config Authority".cyan(),
        config_authority
            .map(|k| k.to_string())
            .unwrap_or_else(|| "None".to_string())
            .bright_white()
    );
    println!(
        "{}: {}",
        "Threshold".cyan(),
        threshold.to_string().bright_green()
    );
    println!(
        "{}: {}",
        "Rent Collector".cyan(),
        rent_collector
            .map(|k| k.to_string())
            .unwrap_or_else(|| "None".to_string())
            .bright_white()
    );
    println!(
        "{}: {}",
        "Members amount".cyan(),
        members.len().to_string().bright_green()
    );
    println!();

    let proceed = Confirm::new()
        .with_prompt("Do you want to proceed?")
        .default(false)
        .interact()?;
    if !proceed {
        println!("{}", "OK, aborting.".bright_red());
        return Err(eyre!("User aborted"));
    }
    println!();

    let rpc_client = create_rpc_client(&rpc_url);

    let progress = ProgressBar::new_spinner().with_message("Sending transaction...");
    progress.enable_steady_tick(Duration::from_millis(100));

    let blockhash = rpc_client
        .get_latest_blockhash()
        .expect("Failed to get blockhash");

    let multisig_key = get_multisig_pda(&create_key.pubkey(), Some(&program_id));

    let program_config_pda = get_program_config_pda(Some(&program_id));

    let program_config = rpc_client
        .get_account(&program_config_pda.0)
        .expect("Failed to fetch program config account");

    let program_config_data = program_config.data.as_slice();

    // Skip the first 8 bytes (discriminator) before deserializing
    let config_data_without_discriminator = &program_config_data[8..];

    let treasury = borsh::from_slice::<ProgramConfig>(config_data_without_discriminator)
        .unwrap()
        .treasury;

    let message = Message::try_compile(
        &transaction_creator,
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(CREATE_MULTISIG_COMPUTE_UNITS),
            ComputeBudgetInstruction::set_compute_unit_price(priority_fee_lamports.unwrap_or(DEFAULT_PRIORITY_FEE)),
            Instruction {
                accounts: MultisigCreateV2Accounts {
                    create_key: create_key.pubkey(),
                    creator: transaction_creator,
                    multisig: multisig_key.0,
                    system_program: solana_system_interface::program::ID,
                    program_config: program_config_pda.0,
                    treasury,
                }
                .to_account_metas(Some(false)),
                data: MultisigCreateV2Data {
                    args: MultisigCreateArgsV2 {
                        config_authority,
                        members,
                        threshold,
                        time_lock: 0,
                        memo: None,
                        rent_collector,
                    },
                }
                .data(),
                program_id,
            },
        ],
        &[],
        blockhash,
    )
    .unwrap();

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(message),
        &[contributor_keypair, create_key as &dyn Signer],
    )
    .expect("Failed to create transaction");

    let signature = send_and_confirm_transaction(&transaction, &rpc_client)?;

    progress.finish_with_message("Transaction confirmed!");

    println!(
        "{} Created Multisig: {}. Signature: {}",
        "✅".bright_green(),
        multisig_key.0.to_string().bright_green(),
        signature.bright_cyan()
    );

    Ok((multisig_key.0, signature))
}

pub async fn create_feature_gate_proposal(
    rpc_urls: Vec<String>,
    program_id: Option<String>,
    multisig_pubkey: Pubkey,
    contributor_keypair: &dyn Signer,
    priority_fee_lamports: Option<u64>,
) -> eyre::Result<()> {
    let program_id =
        program_id.unwrap_or_else(|| SQUADS_PROGRAM_ID_STR.to_string());
    let program_id = Pubkey::from_str(&program_id).expect("Invalid program ID");

    let transaction_creator = contributor_keypair.pubkey();
    let vault_pda = get_vault_pda(&multisig_pubkey, 0, Some(&program_id));
    let feature_gate_id = vault_pda.0; // Use vault as feature gate ID

    println!();
    println!(
        "{}",
        "🚀 Creating feature gate proposals for multisig"
            .bright_yellow()
            .bold()
    );
    println!();
    println!(
        "{}: {}",
        "Multisig".cyan(),
        multisig_pubkey.to_string().bright_white()
    );
    println!(
        "{}: {}",
        "Feature Gate ID".cyan(),
        feature_gate_id.to_string().bright_white()
    );
    println!(
        "{}: {}",
        "Networks".cyan(),
        rpc_urls.len().to_string().bright_green()
    );
    println!();

    let proceed = Confirm::new()
        .with_prompt("Do you want to proceed with creating feature gate proposals?")
        .default(false)
        .interact()?;
    if !proceed {
        println!("{}", "OK, aborting.".bright_red());
        return Err(eyre!("User aborted"));
    }
    println!();

    for (network_idx, rpc_url) in rpc_urls.iter().enumerate() {
        println!(
            "Processing network {} ({}/{})",
            rpc_url.bright_cyan(),
            network_idx + 1,
            rpc_urls.len()
        );

        let rpc_client = create_rpc_client(rpc_url);
        let progress =
            ProgressBar::new_spinner().with_message("Processing feature gate transactions...");
        progress.enable_steady_tick(Duration::from_millis(100));

        let blockhash = rpc_client
            .get_latest_blockhash()
            .expect("Failed to get blockhash");

        // Fetch current multisig state to get next transaction index
        let multisig_account = rpc_client
            .get_account(&multisig_pubkey)
            .expect("Failed to fetch multisig account");

        let multisig_data = multisig_account.data.as_slice();
        let multisig_data_without_discriminator = &multisig_data[8..];
        let multisig: crate::squads::Multisig =
            borsh::from_slice(multisig_data_without_discriminator)
                .expect("Failed to deserialize multisig");

        let base_tx_index = multisig.transaction_index;

        // Create activation transaction and proposal (tx index 1)
        let activation_tx_index = base_tx_index + 1;

        // Create revocation transaction and proposal (tx index 2)
        let revocation_tx_index = base_tx_index + 2;

        // Create transaction messages using utility functions
        let activation_message = crate::utils::create_feature_activation_transaction_message();
        let revocation_message = crate::utils::create_feature_revocation_transaction_message();

        // Transaction 1: Create activation transaction and proposal in one step
        let (activation_combined_message, activation_transaction_pda, activation_proposal_pda) =
            create_transaction_and_proposal_message(
                Some(&program_id),
                &transaction_creator,
                &transaction_creator,
                &multisig_pubkey,
                activation_tx_index,
                0, // vault_index
                activation_message,
                priority_fee_lamports.map(|fee| fee as u32),
                Some(DEFAULT_COMPUTE_UNITS), // compute_unit_limit
                blockhash,
            )?;

        let activation_combined_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(activation_combined_message),
            &[contributor_keypair],
        )
        .expect("Failed to create activation combined transaction");

        let activation_combined_signature =
            send_and_confirm_transaction(&activation_combined_transaction, &rpc_client)?;

        // Transaction 2: Create revocation transaction and proposal in one step
        let (revocation_combined_message, revocation_transaction_pda, revocation_proposal_pda) =
            create_transaction_and_proposal_message(
                Some(&program_id),
                &transaction_creator,
                &transaction_creator,
                &multisig_pubkey,
                revocation_tx_index,
                0, // vault_index
                revocation_message,
                priority_fee_lamports.map(|fee| fee as u32),
                Some(DEFAULT_COMPUTE_UNITS), // compute_unit_limit
                blockhash,
            )?;

        let revocation_combined_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(revocation_combined_message),
            &[contributor_keypair],
        )
        .expect("Failed to create revocation combined transaction");

        let revocation_combined_signature =
            send_and_confirm_transaction(&revocation_combined_transaction, &rpc_client)?;

        progress.finish_with_message("Network completed!");

        println!("✅ Network {} completed:", rpc_url.bright_cyan());
        println!(
            "  Activation Transaction & Proposal ({}): {}",
            activation_tx_index,
            activation_combined_signature.bright_cyan()
        );
        println!(
            "    Transaction PDA: {}",
            activation_transaction_pda.to_string().bright_white()
        );
        println!(
            "    Proposal PDA: {}",
            activation_proposal_pda.to_string().bright_white()
        );
        println!(
            "  Revocation Transaction & Proposal ({}): {}",
            revocation_tx_index,
            revocation_combined_signature.bright_cyan()
        );
        println!(
            "    Transaction PDA: {}",
            revocation_transaction_pda.to_string().bright_white()
        );
        println!(
            "    Proposal PDA: {}",
            revocation_proposal_pda.to_string().bright_white()
        );
        println!();
    }

    println!(
        "{}",
        "🎉 All feature gate proposals created successfully!"
            .bright_green()
            .bold()
    );
    Ok(())
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

    // Derive transaction and proposal PDAs
    let (transaction_pda, _transaction_bump) =
        get_transaction_pda(multisig_address, transaction_index, Some(program_id));
    let (proposal_pda, _proposal_bump) =
        get_proposal_pda(multisig_address, transaction_index, Some(program_id));

    // Create transaction instruction
    let create_transaction_accounts = MultisigCreateTransaction {
        multisig: *multisig_address,
        transaction: transaction_pda,
        creator: *contributor_pubkey,
        rent_payer: *fee_payer_pubkey,
        system_program: solana_system_interface::program::ID,
    };

    // Serialize the TransactionMessage to bytes as expected by the on-chain program
    let transaction_message_bytes = borsh::to_vec(&transaction_message)?;
    
    let create_transaction_data = VaultTransactionCreateArgsData {
        args: VaultTransactionCreateArgs {
            vault_index,
            ephemeral_signers: 0, // No ephemeral signers for basic transactions
            transaction_message: transaction_message_bytes,
            memo: None,
        },
    };

    let create_transaction_instruction = Instruction::new_with_bytes(
        *program_id,
        &create_transaction_data.data(),
        create_transaction_accounts.to_account_metas(None),
    );

    // Create proposal instruction
    let create_proposal_accounts = MultisigCreateProposalAccounts {
        multisig: *multisig_address,
        proposal: proposal_pda,
        creator: *contributor_pubkey,
        rent_payer: *fee_payer_pubkey,
        system_program: solana_system_interface::program::ID,
    };

    let create_proposal_data = MultisigCreateProposalData {
        args: MultisigCreateProposalArgs {
            transaction_index,
            is_draft: false, // Not a draft, ready for voting
        },
    };

    let create_proposal_instruction = Instruction::new_with_bytes(
        *program_id,
        &create_proposal_data.data(),
        create_proposal_accounts.to_account_metas(None),
    );

    // Build instructions list
    let mut instructions = Vec::new();

    // Add compute unit price if specified
    if let Some(microlamports) = priority_fee {
        instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
            microlamports as u64,
        ));
    }

    // Add compute unit limit if specified
    if let Some(units) = compute_unit_limit {
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(units));
    }

    // Add both create transaction and create proposal instructions
    instructions.push(create_transaction_instruction);
    instructions.push(create_proposal_instruction);

    // Create message with fee payer as the payer
    let message = Message::try_compile(fee_payer_pubkey, &instructions, &[], recent_blockhash)?;

    Ok((message, transaction_pda, proposal_pda))
}

pub fn parse_members(member_strings: Vec<String>) -> Result<Vec<Member>, String> {
    member_strings
        .into_iter()
        .map(|s| {
            let parts: Vec<&str> = s.split(',').collect();
            if parts.len() != 2 {
                return Err(
                    "Each entry must be in the format <public_key>,<permission>".to_string()
                );
            }

            let key =
                Pubkey::from_str(parts[0]).map_err(|_| "Invalid public key format".to_string())?;
            let permissions = parts[1]
                .parse::<u8>()
                .map_err(|_| "Invalid permission format".to_string())?;

            Ok(Member {
                key,
                permissions: Permissions { mask: permissions },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
