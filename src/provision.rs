use crate::feature_gate_program::{activate_feature_funded, revoke_pending_activation};
use crate::squads::{
    get_multisig_pda, get_program_config_pda, get_proposal_pda, get_transaction_pda, get_vault_pda,
    Member, MultisigCompiledInstruction, MultisigCreateArgsV2, MultisigCreateProposalAccounts,
    MultisigCreateProposalArgs, MultisigCreateProposalData, MultisigCreateTransaction,
    MultisigCreateTransactionArgs, MultisigCreateTransactionData, MultisigCreateV2Accounts,
    MultisigCreateV2Data, Permissions, ProgramConfig, VaultTransactionMessage,
};
use colored::Colorize;
use dialoguer::Confirm;
use eyre::eyre;
use indicatif::ProgressBar;
use solana_client::client_error::ClientErrorKind;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_request::{RpcError, RpcResponseErrorData};
use solana_client::rpc_response::RpcSimulateTransactionResult;
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::v0::Message;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use solana_transaction::versioned::VersionedTransaction;
use std::str::FromStr;
use std::time::Duration;

pub fn send_and_confirm_transaction(
    transaction: &VersionedTransaction,
    rpc_client: &RpcClient,
) -> eyre::Result<String> {
    // Try to send and confirm the transaction
    match rpc_client.send_and_confirm_transaction(transaction) {
        Ok(signature) => {
            println!(
                "Transaction confirmed: {}\n\n",
                signature.to_string().bright_green()
            );
            Ok(signature.to_string())
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

            Err(eyre!(
                "Transaction failed: {}",
                err.to_string().bright_red()
            ))
        }
    }
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
        program_id.unwrap_or_else(|| "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf".to_string());
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

    let rpc_client = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());

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
            ComputeBudgetInstruction::set_compute_unit_limit(50_000),
            ComputeBudgetInstruction::set_compute_unit_price(priority_fee_lamports.unwrap_or(5000)),
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
        program_id.unwrap_or_else(|| "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf".to_string());
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

        let rpc_client = RpcClient::new(rpc_url.clone());
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

        // Create activation transaction (tx index 1)
        let activation_tx_index = base_tx_index + 1;
        let activation_transaction_pda =
            get_transaction_pda(&multisig_pubkey, activation_tx_index, Some(&program_id));
        let activation_proposal_pda =
            get_proposal_pda(&multisig_pubkey, activation_tx_index, Some(&program_id));

        // Create revocation transaction (tx index 2)
        let revocation_tx_index = base_tx_index + 2;
        let revocation_transaction_pda =
            get_transaction_pda(&multisig_pubkey, revocation_tx_index, Some(&program_id));
        let revocation_proposal_pda =
            get_proposal_pda(&multisig_pubkey, revocation_tx_index, Some(&program_id));

        // Create feature activation instructions
        let activation_ixs = activate_feature_funded(&feature_gate_id);

        // Create feature revocation instruction
        let revocation_ix = revoke_pending_activation(&feature_gate_id);

        // Build proper VersionedTransaction for activation to get correct message structure
        let activation_temp_message = Message::try_compile(
            &vault_pda.0, // Use vault as the fee payer for the inner transaction
            &activation_ixs,
            &[],
            blockhash,
        )
        .unwrap();

        let empty_signers: &[&dyn Signer] = &[];
        let activation_temp_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(activation_temp_message.clone()),
            empty_signers,
        )
        .expect("Failed to create temporary activation transaction");

        // Build proper VersionedTransaction for revocation to get correct message structure
        let revocation_temp_message = Message::try_compile(
            &vault_pda.0, // Use vault as the fee payer for the inner transaction
            &[revocation_ix.clone()],
            &[],
            blockhash,
        )
        .unwrap();

        let revocation_temp_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(revocation_temp_message.clone()),
            empty_signers,
        )
        .expect("Failed to create temporary revocation transaction");

        // Extract message information from the VersionedTransactions
        let activation_message = match &activation_temp_transaction.message {
            VersionedMessage::V0(msg) => {
                let header = &msg.header;
                VaultTransactionMessage {
                    num_signers: header.num_required_signatures,
                    num_writable_signers: header.num_readonly_signed_accounts,
                    num_writable_non_signers: header.num_readonly_unsigned_accounts,
                    account_keys: msg.account_keys.clone(),
                    instructions: msg
                        .instructions
                        .iter()
                        .map(|ix| MultisigCompiledInstruction {
                            program_id_index: ix.program_id_index,
                            account_indexes: ix.accounts.clone(),
                            data: ix.data.clone(),
                        })
                        .collect(),
                    address_table_lookups: msg
                        .address_table_lookups
                        .iter()
                        .map(|lookup| crate::squads::MultisigMessageAddressTableLookup {
                            account_key: lookup.account_key,
                            writable_indexes: lookup.writable_indexes.clone(),
                            readonly_indexes: lookup.readonly_indexes.clone(),
                        })
                        .collect(),
                }
            }
            VersionedMessage::Legacy(_) => panic!("Expected V0 message"),
        };

        let revocation_message = match &revocation_temp_transaction.message {
            VersionedMessage::V0(msg) => {
                let header = &msg.header;
                VaultTransactionMessage {
                    num_signers: header.num_required_signatures,
                    num_writable_signers: header.num_readonly_signed_accounts,
                    num_writable_non_signers: header.num_readonly_unsigned_accounts,
                    account_keys: msg.account_keys.clone(),
                    instructions: msg
                        .instructions
                        .iter()
                        .map(|ix| MultisigCompiledInstruction {
                            program_id_index: ix.program_id_index,
                            account_indexes: ix.accounts.clone(),
                            data: ix.data.clone(),
                        })
                        .collect(),
                    address_table_lookups: msg
                        .address_table_lookups
                        .iter()
                        .map(|lookup| crate::squads::MultisigMessageAddressTableLookup {
                            account_key: lookup.account_key,
                            writable_indexes: lookup.writable_indexes.clone(),
                            readonly_indexes: lookup.readonly_indexes.clone(),
                        })
                        .collect(),
                }
            }
            VersionedMessage::Legacy(_) => panic!("Expected V0 message"),
        };

        // Transaction 1: Create activation transaction
        let create_activation_tx_message = Message::try_compile(
            &transaction_creator,
            &[
                ComputeBudgetInstruction::set_compute_unit_price(
                    priority_fee_lamports.unwrap_or(5000),
                ),
                ComputeBudgetInstruction::set_compute_unit_limit(300_000),
                Instruction {
                    accounts: MultisigCreateTransaction {
                        multisig: multisig_pubkey,
                        transaction: activation_transaction_pda.0,
                        creator: transaction_creator,
                        rent_payer: transaction_creator,
                        system_program: solana_system_interface::program::ID,
                    }
                    .to_account_metas(None),
                    data: MultisigCreateTransactionData {
                        args: MultisigCreateTransactionArgs {
                            vault_index: 0,
                            num_ephemeral_signers: 0,
                            transaction_message: activation_message,
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

        let activation_tx_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(create_activation_tx_message),
            &[contributor_keypair],
        )
        .expect("Failed to create activation transaction");

        let activation_tx_signature =
            send_and_confirm_transaction(&activation_tx_transaction, &rpc_client)?;

        // Transaction 2: Create activation proposal
        let create_activation_proposal_message = Message::try_compile(
            &transaction_creator,
            &[
                ComputeBudgetInstruction::set_compute_unit_price(
                    priority_fee_lamports.unwrap_or(5000),
                ),
                ComputeBudgetInstruction::set_compute_unit_limit(300_000),
                Instruction {
                    accounts: MultisigCreateProposalAccounts {
                        multisig: multisig_pubkey,
                        proposal: activation_proposal_pda.0,
                        creator: transaction_creator,
                        rent_payer: transaction_creator,
                        system_program: solana_system_interface::program::ID,
                    }
                    .to_account_metas(None),
                    data: MultisigCreateProposalData {
                        args: MultisigCreateProposalArgs {
                            transaction_index: activation_tx_index,
                            is_draft: false,
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

        let activation_proposal_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(create_activation_proposal_message),
            &[contributor_keypair],
        )
        .expect("Failed to create activation proposal transaction");

        let activation_proposal_signature =
            send_and_confirm_transaction(&activation_proposal_transaction, &rpc_client)?;

        // Transaction 3: Create revocation transaction
        let create_revocation_tx_message = Message::try_compile(
            &transaction_creator,
            &[
                ComputeBudgetInstruction::set_compute_unit_price(
                    priority_fee_lamports.unwrap_or(5000),
                ),
                ComputeBudgetInstruction::set_compute_unit_limit(300_000),
                Instruction {
                    accounts: MultisigCreateTransaction {
                        multisig: multisig_pubkey,
                        transaction: revocation_transaction_pda.0,
                        creator: transaction_creator,
                        rent_payer: transaction_creator,
                        system_program: solana_system_interface::program::ID,
                    }
                    .to_account_metas(None),
                    data: MultisigCreateTransactionData {
                        args: MultisigCreateTransactionArgs {
                            vault_index: 0,
                            num_ephemeral_signers: 0,
                            transaction_message: revocation_message,
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

        let revocation_tx_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(create_revocation_tx_message),
            &[contributor_keypair],
        )
        .expect("Failed to create revocation transaction");

        let revocation_tx_signature =
            send_and_confirm_transaction(&revocation_tx_transaction, &rpc_client)?;

        // Transaction 4: Create revocation proposal
        let create_revocation_proposal_message = Message::try_compile(
            &transaction_creator,
            &[
                ComputeBudgetInstruction::set_compute_unit_price(
                    priority_fee_lamports.unwrap_or(5000),
                ),
                ComputeBudgetInstruction::set_compute_unit_limit(300_000),
                Instruction {
                    accounts: MultisigCreateProposalAccounts {
                        multisig: multisig_pubkey,
                        proposal: revocation_proposal_pda.0,
                        creator: transaction_creator,
                        rent_payer: transaction_creator,
                        system_program: solana_system_interface::program::ID,
                    }
                    .to_account_metas(None),
                    data: MultisigCreateProposalData {
                        args: MultisigCreateProposalArgs {
                            transaction_index: revocation_tx_index,
                            is_draft: false,
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

        let revocation_proposal_transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(create_revocation_proposal_message),
            &[contributor_keypair],
        )
        .expect("Failed to create revocation proposal transaction");

        let revocation_proposal_signature =
            send_and_confirm_transaction(&revocation_proposal_transaction, &rpc_client)?;

        progress.finish_with_message("Network completed!");

        println!("✅ Network {} completed:", rpc_url.bright_cyan());
        println!(
            "  Activation Transaction ({}): {}",
            activation_tx_index,
            activation_tx_signature.bright_cyan()
        );
        println!(
            "  Activation Proposal ({}): {}",
            activation_tx_index,
            activation_proposal_signature.bright_cyan()
        );
        println!(
            "  Revocation Transaction ({}): {}",
            revocation_tx_index,
            revocation_tx_signature.bright_cyan()
        );
        println!(
            "  Revocation Proposal ({}): {}",
            revocation_tx_index,
            revocation_proposal_signature.bright_cyan()
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
