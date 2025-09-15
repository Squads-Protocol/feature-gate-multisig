use eyre::Result;
use colored::Colorize;
use serde::Serialize;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::str::FromStr;

use crate::{
    provision::create_approve_activation_transaction_message,
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

    let transaction = VersionedTransaction::try_new(
        VersionedMessage::V0(transaction_message),
        &[&fee_payer_keypair.unwrap()],
    )?;
    let serialized_transaction = bincode::serialize(&transaction)?;

    let transaction_encoding = choose_transaction_encoding()?;

    let transaction_encoded = match transaction_encoding {
        TransactionEncoding::Base58 => {
            let transaction_encoded = bs58::encode(&serialized_transaction).into_string();
            transaction_encoded
        }
        TransactionEncoding::Base64 => {
            let transaction_encoded = base64::encode(&serialized_transaction);
            transaction_encoded
        }
    };

    println!("\n{}", "Encoded transaction:".bright_green().bold());
    println!("{}", transaction_encoded.bright_green());
    Ok(())
}
