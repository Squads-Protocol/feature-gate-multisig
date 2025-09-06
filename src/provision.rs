use crate::squads::{get_multisig_pda, get_program_config_pda, Member, Permissions, ProgramConfig, MultisigCreateV2Accounts, MultisigCreateV2Data, MultisigCreateArgsV2};
use colored::Colorize;
use dialoguer::Confirm;
use eyre::eyre;
use indicatif::ProgressBar;
use solana_client::client_error::ClientErrorKind;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_request::{RpcError, RpcResponseErrorData};
use solana_client::rpc_response::RpcSimulateTransactionResult;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::message::v0::Message;
use solana_sdk::message::VersionedMessage;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::system_program;
use solana_sdk::transaction::VersionedTransaction;
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

            Err(eyre!("Transaction failed: {}", err.to_string().bright_red()))
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
    let program_id = program_id
        .unwrap_or_else(|| "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf".to_string());
    let program_id = Pubkey::from_str(&program_id).expect("Invalid program ID");

    let transaction_creator = contributor_keypair.pubkey();

    println!();
    println!(
        "{}",
        "👀 You're about to create a multisig, please review the details:".bright_yellow().bold()
    );
    println!();
    println!("{}: {}", "RPC Cluster URL".cyan(), rpc_url.bright_white());
    println!("{}: {}", "Program ID".cyan(), program_id.to_string().bright_white());
    println!("{}: {}", "Your Public Key".cyan(), transaction_creator.to_string().bright_white());
    println!();
    println!("{}", "⚙️ Config Parameters".bright_yellow().bold());
    println!();
    println!(
        "{}: {}",
        "Config Authority".cyan(),
        config_authority
            .map(|k| k.to_string())
            .unwrap_or_else(|| "None".to_string()).bright_white()
    );
    println!("{}: {}", "Threshold".cyan(), threshold.to_string().bright_green());
    println!(
        "{}: {}",
        "Rent Collector".cyan(),
        rent_collector
            .map(|k| k.to_string())
            .unwrap_or_else(|| "None".to_string()).bright_white()
    );
    println!("{}: {}", "Members amount".cyan(), members.len().to_string().bright_green());
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

    let rpc_client = RpcClient::new(rpc_url);

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
            ComputeBudgetInstruction::set_compute_unit_price(
                priority_fee_lamports.unwrap_or(5000),
            ),
            Instruction {
                accounts: MultisigCreateV2Accounts {
                    create_key: create_key.pubkey(),
                    creator: transaction_creator,
                    multisig: multisig_key.0,
                    system_program: system_program::id(),
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
