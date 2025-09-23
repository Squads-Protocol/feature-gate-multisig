use colored::Colorize;
use eyre::Result;
use serde::Serialize;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::str::FromStr;

use crate::{
    output,
    provision::{
        create_approve_activation_transaction_message,
        create_execute_activation_transaction_message,
    },
    utils::{
        choose_network_from_config, choose_transaction_encoding, load_fee_payer_keypair, Config,
        TransactionEncoding,
    },
};

pub async fn approve_feature_gate_activation_proposal(
    config: &Config,
    feature_gate_multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    program_id: Option<Pubkey>,
) -> Result<()> {
    let program_id = program_id
        .unwrap_or_else(|| Pubkey::from_str_const("SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf"));

    let fee_payer_keypair = load_fee_payer_keypair(config, Some(fee_payer_path))?;
    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = RpcClient::new(rpc_url);
    let blockhash = rpc_client.get_latest_blockhash().await?;

    let transaction_message = create_approve_activation_transaction_message(
        &program_id,
        &feature_gate_multisig_address,
        &voting_key,
        &fee_payer_keypair.as_ref().unwrap().pubkey(),
        blockhash,
    )
    .map_err(|e| {
        eyre::eyre!(
            "Failed to create approve activation transaction message: {}",
            e
        )
    })?;

    let mut transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(transaction_message),
        &[&fee_payer_keypair.unwrap()],
    )?;
    transaction.signatures.push(Signature::default());
    let serialized_transaction = bincode::serialize(&transaction)?;

    let transaction_encoded_bs58 = bs58::encode(&serialized_transaction).into_string();
    let transaction_encoded_base64 = base64::encode(&serialized_transaction);

    output::Output::header("Encoded Transactions:");
    output::Output::separator();
    output::Output::field("Base58:", &transaction_encoded_bs58);
    output::Output::separator();
    output::Output::field("Base64:", &transaction_encoded_base64);
    Ok(())
}

pub async fn execute_feature_gate_activation_proposal(
    config: &Config,
    feature_gate_multisig_address: Pubkey,
    voting_key: Pubkey,
    fee_payer_path: String,
    program_id: Option<Pubkey>,
) -> Result<()> {
    let program_id = program_id
        .unwrap_or_else(|| Pubkey::from_str_const("SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf"));

    let fee_payer_keypair = load_fee_payer_keypair(config, Some(fee_payer_path))?;
    let rpc_url = choose_network_from_config(config)?;
    let rpc_client = RpcClient::new(rpc_url);
    let blockhash = rpc_client.get_latest_blockhash().await?;

    let transaction_message = create_execute_activation_transaction_message(
        &program_id,
        &feature_gate_multisig_address,
        &voting_key,
        &fee_payer_keypair.as_ref().unwrap().pubkey(),
        &rpc_client,
        blockhash,
    )
    .await?;

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(transaction_message),
        &[&fee_payer_keypair.unwrap()],
    )?;
    let serialized_transaction = bincode::serialize(&transaction)?;

    output::Output::header("Encoded Transactions:");

    let transaction_encoded_bs58 = bs58::encode(&serialized_transaction).into_string();
    let transaction_encoded_base64 = base64::encode(&serialized_transaction);

    output::Output::field("Base58:", &transaction_encoded_bs58);
    output::Output::field("Base64:", &transaction_encoded_base64);

    Ok(())
}
