use crate::squads::{get_vault_pda, Multisig};
use crate::utils::*;
use anyhow::Result;
use colored::*;
use solana_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use std::str::FromStr;
use tabled::{settings::Style, Table, Tabled};

pub async fn show_command(config: &Config, address: Option<String>) -> Result<()> {
    let address = if let Some(addr) = address {
        // Validate provided address
        match Pubkey::from_str(&addr) {
            Ok(_) => addr,
            Err(_) => {
                println!(
                    "{} Invalid address format: {}",
                    "❌".bright_red(),
                    addr.bright_red()
                );
                return Err(anyhow::anyhow!("Invalid multisig address format"));
            }
        }
    } else {
        validate_pubkey_with_retry("Enter multisig address:")?.to_string()
    };
    show_multisig(config, &address).await
}

async fn show_multisig(config: &Config, address: &str) -> Result<()> {
    // Parse the multisig address
    let multisig_pubkey = Pubkey::from_str(address)
        .map_err(|_| anyhow::anyhow!("Invalid multisig address format"))?;

    println!(
        "{}",
        "🔍 Fetching multisig details...".bright_yellow().bold()
    );
    println!();

    // Try all configured networks until we find the account
    let mut account_data = None;
    let mut successful_rpc_url = None;
    let mut last_error = None;

    let networks_to_try = if !config.networks.is_empty() {
        config.networks.clone()
    } else if !config.network.is_empty() {
        vec![config.network.clone()]
    } else {
        vec!["https://api.devnet.solana.com".to_string()]
    };

    println!("Available networks to search:");
    for (i, network) in networks_to_try.iter().enumerate() {
        println!("  {}: {}", i + 1, network);
    }
    println!();

    for rpc_url in &networks_to_try {
        println!("🌐 Trying network: {}", rpc_url.bright_white());

        let rpc_client = RpcClient::new(rpc_url);
        match rpc_client.get_account_data(&multisig_pubkey) {
            Ok(data) => {
                println!("✅ Found account on: {}", rpc_url.bright_green());
                account_data = Some(data);
                successful_rpc_url = Some(rpc_url.clone());
                break;
            }
            Err(e) => {
                let error_str = e.to_string();
                if error_str.contains("AccountNotFound")
                    || error_str.contains("could not find account")
                {
                    println!("❌ Account not found on: {}", rpc_url.bright_red());
                    last_error = Some(format!("Account not found: {}. This address may not exist on any of the configured networks or may not be a multisig account.", multisig_pubkey));
                } else {
                    println!("❌ Error querying {}: {}", rpc_url.bright_red(), e);
                    last_error = Some(format!("Failed to query networks: {}", e));
                }
            }
        }
    }

    let (rpc_url, account_data) = match (successful_rpc_url, account_data) {
        (Some(url), Some(data)) => (url, data),
        _ => {
            return Err(anyhow::anyhow!(
                "{}",
                last_error.unwrap_or_else(
                    || "Failed to find account on any configured network".to_string()
                )
            ));
        }
    };

    println!();
    println!("📡 Using network: {}", rpc_url.bright_white());
    println!(
        "🎯 Multisig address: {}",
        multisig_pubkey.to_string().bright_white()
    );
    println!();

    if account_data.len() < 8 {
        return Err(anyhow::anyhow!(
            "Account data too small to be a valid multisig"
        ));
    }

    println!("📊 Account data length: {} bytes", account_data.len());

    // Strip the first 8 bytes (discriminator) and deserialize
    let multisig: Multisig = match borsh::BorshDeserialize::deserialize(&mut &account_data[8..]) {
        Ok(ms) => ms,
        Err(e) if e.to_string().contains("Not all bytes read") => {
            // This is expected for accounts with pre-allocated member slots (padding)
            // Try to deserialize only the data we need, ignoring trailing bytes
            use borsh::BorshDeserialize;
            let mut slice = &account_data[8..];
            Multisig::deserialize(&mut slice)
                .map_err(|e2| anyhow::anyhow!("Failed to deserialize multisig data even with partial read: {}. This account may not be a valid Squads multisig.", e2))?
        },
        Err(e) => return Err(anyhow::anyhow!("Failed to deserialize multisig data: {}. This account may not be a valid Squads multisig.", e)),
    };

    println!("✅ Multisig deserialized successfully!");

    // Display the multisig details
    display_multisig_details(&multisig, &multisig_pubkey)?;

    Ok(())
}

fn display_multisig_details(multisig: &Multisig, address: &Pubkey) -> Result<()> {
    println!("{}", "📋 MULTISIG DETAILS".bright_green().bold());
    println!("{}", "═".repeat(80).bright_green());
    println!();

    // Basic info table
    #[derive(Tabled)]
    struct MultisigInfo {
        #[tabled(rename = "Property")]
        property: String,
        #[tabled(rename = "Value")]
        value: String,
    }

    let info_data = vec![
        MultisigInfo {
            property: "Multisig Address".to_string(),
            value: address.to_string(),
        },
        MultisigInfo {
            property: "Create Key".to_string(),
            value: multisig.create_key.to_string(),
        },
        MultisigInfo {
            property: "Config Authority".to_string(),
            value: multisig.config_authority.to_string(),
        },
        MultisigInfo {
            property: "Threshold".to_string(),
            value: format!("{} of {}", multisig.threshold, multisig.members.len()),
        },
        MultisigInfo {
            property: "Time Lock (seconds)".to_string(),
            value: multisig.time_lock.to_string(),
        },
        MultisigInfo {
            property: "Transaction Index".to_string(),
            value: multisig.transaction_index.to_string(),
        },
        MultisigInfo {
            property: "Stale Transaction Index".to_string(),
            value: multisig.stale_transaction_index.to_string(),
        },
        MultisigInfo {
            property: "Rent Collector".to_string(),
            value: multisig
                .rent_collector
                .map(|r| r.to_string())
                .unwrap_or_else(|| "None".to_string()),
        },
        MultisigInfo {
            property: "PDA Bump".to_string(),
            value: multisig.bump.to_string(),
        },
    ];

    let mut info_table = Table::new(info_data);
    info_table.with(Style::rounded());
    println!("{}", info_table);
    println!();

    // Members table
    #[derive(Tabled)]
    struct MemberInfo {
        #[tabled(rename = "#")]
        index: usize,
        #[tabled(rename = "Public Key")]
        pubkey: String,
        #[tabled(rename = "Permissions")]
        permissions: String,
        #[tabled(rename = "Bitmask")]
        bitmask: u8,
    }

    println!(
        "{} ({} total)",
        "👥 MEMBERS".bright_blue().bold(),
        multisig.members.len()
    );
    println!();

    let member_data: Vec<MemberInfo> = multisig
        .members
        .iter()
        .enumerate()
        .map(|(i, member)| {
            let perms = decode_permissions(member.permissions.mask);
            MemberInfo {
                index: i + 1,
                pubkey: member.key.to_string(),
                permissions: if perms.is_empty() {
                    "None".to_string()
                } else {
                    perms.join(", ")
                },
                bitmask: member.permissions.mask,
            }
        })
        .collect();

    let mut members_table = Table::new(member_data);
    members_table.with(Style::rounded());
    println!("{}", members_table);
    println!();

    // Calculate and display vault addresses for common indices
    println!("{}", "🏦 VAULT ADDRESSES".bright_cyan().bold());
    println!();

    #[derive(Tabled)]
    struct VaultInfo {
        #[tabled(rename = "Index")]
        index: u8,
        #[tabled(rename = "Vault Address")]
        address: String,
        #[tabled(rename = "Description")]
        description: String,
    }

    let vault_data = vec![
        VaultInfo {
            index: 0,
            address: get_vault_pda(address, 0, None).0.to_string(),
            description: "Default vault (commonly used for feature gates)".to_string(),
        },
        VaultInfo {
            index: 1,
            address: get_vault_pda(address, 1, None).0.to_string(),
            description: "Vault #1".to_string(),
        },
        VaultInfo {
            index: 2,
            address: get_vault_pda(address, 2, None).0.to_string(),
            description: "Vault #2".to_string(),
        },
    ];

    let mut vault_table = Table::new(vault_data);
    vault_table.with(Style::rounded());
    println!("{}", vault_table);
    println!();

    println!(
        "{}",
        "✅ Multisig details retrieved successfully!".bright_green()
    );

    Ok(())
}