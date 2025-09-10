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

pub fn create_transaction_and_proposal_message(
    program_id: Option<&Pubkey>,
    fee_payer_pubkey: &Pubkey,
    contributor_pubkey: &Pubkey,
    multisig_address: &Pubkey,
    transaction_index: u64,
    vault_index: u8,
    transaction_message: VaultTransactionMessage,
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

    let create_transaction_data = MultisigCreateTransactionData {
        args: MultisigCreateTransactionArgs {
            vault_index,
            num_ephemeral_signers: 0, // No ephemeral signers for basic transactions
            transaction_message,
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
mod tests {
    use super::*;
    use borsh::BorshDeserialize;

    fn create_test_transaction_message() -> VaultTransactionMessage {
        use crate::feature_gate_program::create_feature_activation;

        // Create feature activation instructions for a test feature
        let feature_id = Pubkey::new_unique();
        let funding_address = Pubkey::new_unique();
        let instructions = create_feature_activation(&feature_id, &funding_address);

        // Build account keys list for the message
        let mut account_keys = vec![
            funding_address,                      // 0: Funding address (signer, writable)
            feature_id,                           // 1: Feature account (writable)
            solana_system_interface::program::ID, // 2: System program
            crate::feature_gate_program::FEATURE_GATE_PROGRAM_ID, // 3: Feature gate program
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

            compiled_instructions.push(MultisigCompiledInstruction {
                program_id_index,
                account_indexes,
                data: instruction.data,
            });
        }

        VaultTransactionMessage {
            num_signers: 1,              // funding_address is the signer
            num_writable_signers: 1,     // funding_address is writable signer
            num_writable_non_signers: 1, // feature_id is writable non-signer
            account_keys,
            instructions: compiled_instructions,
            address_table_lookups: vec![],
        }
    }

    #[test]
    fn test_create_transaction_data_serialization() {
        let transaction_message = create_test_transaction_message();

        let create_transaction_data = MultisigCreateTransactionData {
            args: MultisigCreateTransactionArgs {
                vault_index: 0,
                num_ephemeral_signers: 0,
                transaction_message: transaction_message.clone(),
            },
        };

        // Serialize the data
        let serialized_data = create_transaction_data.data();

        // Check that it starts with the correct discriminator
        assert_eq!(
            &serialized_data[0..8],
            crate::squads::CREATE_TRANSACTION_DISCRIMINATOR
        );

        // Test deserialization of the args portion
        let args_data = &serialized_data[8..];
        let deserialized_args = MultisigCreateTransactionArgs::try_from_slice(args_data).unwrap();

        assert_eq!(deserialized_args.vault_index, 0);
        assert_eq!(deserialized_args.num_ephemeral_signers, 0);
        assert_eq!(
            deserialized_args.transaction_message.num_signers,
            transaction_message.num_signers
        );
        assert_eq!(
            deserialized_args.transaction_message.account_keys.len(),
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
        let serialized_data = create_proposal_data.data();

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
        let deserialized = VaultTransactionMessage::try_from_slice(&serialized).unwrap();

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
        let (transaction_pda, transaction_bump) =
            get_transaction_pda(&multisig_address, transaction_index, None);
        assert!(transaction_bump <= 255); // Valid bump seed

        // Test proposal PDA derivation
        let (proposal_pda, proposal_bump) =
            get_proposal_pda(&multisig_address, transaction_index, None);
        assert!(proposal_bump <= 255); // Valid bump seed

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

        let tx_metas = create_transaction_accounts.to_account_metas(None);
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

        let proposal_metas = create_proposal_accounts.to_account_metas(None);
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
        // Should have 3: priority fee + create transaction + create proposal
        assert_eq!(message.instructions.len(), 3);

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

        // Verify we have 3 compiled instructions (transfer, allocate, assign)
        assert_eq!(transaction_message.instructions.len(), 3);

        // Verify account structure
        assert!(transaction_message.account_keys.len() >= 4); // At least: funding, feature, system, feature_gate_program

        // Verify signer counts
        assert_eq!(transaction_message.num_signers, 1);
        assert_eq!(transaction_message.num_writable_signers, 1);
        assert_eq!(transaction_message.num_writable_non_signers, 1);

        // First account should be the funding address (signer)
        // Second account should be the feature account (writable non-signer)
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
