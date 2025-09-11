use crate::feature_gate_program::create_feature_activation;
use crate::provision::create_transaction_and_proposal_message;
use crate::squads::{CompiledInstruction, Member, Permissions, TransactionMessage};
use anyhow::Result;
use colored::*;
use dirs;
use inquire::{Confirm, Text};
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_message::{VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::{EncodableKey, Signer};
use solana_transaction::versioned::VersionedTransaction;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub threshold: u16,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub networks: Vec<String>,
    // Legacy single network field for backward compatibility
    #[serde(default)]
    pub network: String,
    // Keep for backward compatibility but don't use
    #[serde(default, skip_serializing)]
    pub signers: Vec<Vec<String>>,
    #[serde(default)]
    pub fee_payer_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: 1,
            members: Vec::new(),
            networks: vec!["https://api.devnet.solana.com".to_string()],
            network: "https://api.devnet.solana.com".to_string(),
            signers: vec![vec![]],
            fee_payer_path: None,
        }
    }
}

#[derive(Debug)]
pub struct DeploymentResult {
    pub rpc_url: String,
    pub multisig_address: Pubkey,
    pub vault_address: Pubkey,
    pub transaction_signature: String,
}

// Config management functions
pub fn get_config_path() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    Ok(home_dir
        .join(".feature-gate-multisig-tool")
        .join("config.json"))
}

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;

    if !config_path.exists() {
        let config = Config::default();
        save_config(&config)?;
        return Ok(config);
    }

    let config_str = fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to read config file: {}", e))?;

    let config: Config = serde_json::from_str(&config_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

    Ok(config)
}

pub fn save_config(config: &Config) -> Result<()> {
    let config_path = get_config_path()?;

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Failed to create config directory: {}", e))?;
    }

    let config_str = serde_json::to_string_pretty(config)
        .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;

    fs::write(&config_path, config_str)
        .map_err(|e| anyhow::anyhow!("Failed to write config file: {}", e))?;

    Ok(())
}

// Member management functions
pub fn parse_saved_members(config: &Config) -> Vec<Member> {
    let mut parsed_members = Vec::new();
    for member_str in &config.members {
        match Pubkey::from_str(member_str) {
            Ok(pubkey) => {
                parsed_members.push(Member {
                    key: pubkey,
                    permissions: Permissions { mask: 7 }, // Full permissions for saved members
                });
            }
            Err(_) => {
                println!(
                    "  {} Invalid saved member key: {}, skipping...",
                    "⚠️".bright_yellow(),
                    member_str
                );
            }
        }
    }
    parsed_members
}

pub fn collect_members_interactively() -> Result<Vec<Member>> {
    let mut interactive_members = Vec::new();

    loop {
        let add_member = Confirm::new("Add a member?").with_default(true).prompt()?;

        if !add_member {
            break;
        }

        match validate_pubkey_with_retry("Enter member public key:") {
            Ok(member_key) => {
                interactive_members.push(Member {
                    key: member_key,
                    permissions: Permissions { mask: 7 },
                });
                println!(
                    "  {} Added member: {} ({})",
                    "✓".bright_green(),
                    member_key.to_string().bright_white(),
                    "Initiate, Vote, Execute".bright_cyan()
                );
            }
            Err(e) => {
                println!(
                    "  {} Failed to add member: {}",
                    "❌".bright_red(),
                    e.to_string().bright_red()
                );
                continue;
            }
        }
    }

    Ok(interactive_members)
}

pub fn review_and_collect_configuration(config: &Config, threshold: u16) -> Result<(u16, Vec<Member>)> {
    let use_saved_config = review_config(config)?;

    if use_saved_config {
        println!("{} Using saved configuration", "✅".bright_green());
        let parsed_members = parse_saved_members(config);
        Ok((config.threshold, parsed_members))
    } else {
        println!("{} Collecting members interactively", "🔄".bright_cyan());
        let interactive_members = collect_members_interactively()?;
        Ok((threshold, interactive_members))
    }
}

// Keypair management functions
pub fn expand_tilde_path(path: &str) -> Result<String> {
    if path.starts_with("~/") {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        Ok(home.join(&path[2..]).to_string_lossy().to_string())
    } else {
        Ok(path.to_string())
    }
}

pub fn load_fee_payer_keypair(config: &Config, keypair_path: Option<String>) -> Result<Option<Keypair>> {
    if let Some(path) = keypair_path {
        println!(
            "{} Loading fee payer keypair from CLI: {}",
            "💰".bright_blue(),
            path.bright_white()
        );
        let keypair = Keypair::read_from_file(&path)
            .map_err(|e| anyhow::anyhow!("Failed to load keypair from {}: {}", path, e))?;
        Ok(Some(keypair))
    } else if let Some(path) = &config.fee_payer_path {
        println!(
            "{} Loading fee payer keypair from config: {}",
            "💰".bright_blue(),
            path.bright_white()
        );
        let keypair = Keypair::read_from_file(path).map_err(|e| {
            anyhow::anyhow!("Failed to load keypair from config path {}: {}", path, e)
        })?;
        Ok(Some(keypair))
    } else {
        println!("{} No fee payer keypair provided", "⚠️".bright_yellow());
        Ok(None)
    }
}

// CLI input helpers
pub fn prompt_for_threshold(config: &Config) -> Result<u16> {
    loop {
        let input = Text::new(&format!(
            "Enter threshold (required signatures) [{}]:",
            config.threshold
        ))
        .prompt()
        .unwrap_or_default();

        match validate_threshold(&input, 10, config.threshold) {
            Ok(t) => return Ok(t),
            Err(e) => {
                println!("  {} {}", "❌".bright_red(), e.to_string().bright_red());
                continue;
            }
        }
    }
}

pub fn prompt_for_fee_payer_path(config: &Config) -> Result<String> {
    let default_feepayer = config
        .fee_payer_path
        .as_deref()
        .unwrap_or("~/.config/solana/id.json");

    let feepayer_path = Text::new("Enter fee payer keypair file path:")
        .with_default(default_feepayer)
        .prompt()?;

    expand_tilde_path(&feepayer_path)
}

pub fn prompt_for_network(config: &Config) -> Result<String> {
    let default_network = if !config.networks.is_empty() {
        &config.networks[0]
    } else {
        &config.network
    };

    loop {
        let input = Text::new("Enter RPC URL for deployment:")
            .with_default(default_network)
            .prompt()?;

        match validate_rpc_url(&input) {
            Ok(url) => return Ok(url),
            Err(e) => {
                println!("  {} {}", "❌".bright_red(), e.to_string().bright_red());
                let retry = Confirm::new("Try again?").with_default(true).prompt()?;
                if !retry {
                    return Err(anyhow::anyhow!("User cancelled network entry"));
                }
            }
        }
    }
}

// Display functions
pub fn display_final_configuration(
    contributor_pubkey: &Pubkey,
    create_key: &Pubkey,
    fee_payer_keypair: &Option<Keypair>,
    threshold: u16,
    members: &[Member],
) {
    println!("\n{}", "📋 Final Configuration:".bright_yellow().bold());
    println!(
        "  {}: {}",
        "Contributor public key".cyan(),
        contributor_pubkey.to_string().bright_white()
    );
    println!(
        "  {}: {}",
        "Create key".cyan(),
        create_key.to_string().bright_white()
    );
    if let Some(fee_payer) = fee_payer_keypair {
        println!(
            "  {}: {}",
            "Fee payer".cyan(),
            fee_payer.pubkey().to_string().bright_green()
        );
    } else {
        println!(
            "  {}: {} (same as contributor)",
            "Fee payer".cyan(),
            contributor_pubkey.to_string().bright_yellow()
        );
    }
    println!(
        "  {}: {}",
        "Threshold".cyan(),
        threshold.to_string().bright_green()
    );

    println!("\n{}", "👥 All Members:".bright_yellow().bold());
    println!(
        "  {} Contributor: {} ({})",
        "✓".bright_green(),
        contributor_pubkey.to_string().bright_white(),
        "Initiate".bright_cyan()
    );

    for (i, member) in members.iter().skip(1).enumerate() {
        let perms = decode_permissions(member.permissions.mask);
        println!(
            "  {} Member {}: {} ({})",
            "✓".bright_green(),
            i + 1,
            member.key.to_string().bright_white(),
            perms.join(", ").bright_cyan()
        );
    }

    println!(
        "\n{} {}",
        "📊 Total members:".bright_yellow().bold(),
        members.len().to_string().bright_green()
    );
    println!();
}

pub fn display_deployment_info(
    network_index: usize,
    total_networks: usize,
    rpc_url: &str,
    create_key: &Pubkey,
    contributor_pubkey: &Pubkey,
    multisig_address: &Pubkey,
    vault_address: &Pubkey,
    members: &[Member],
) {
    if total_networks > 1 {
        println!(
            "\n{} Deployment {} of {} to: {}",
            "📡".bright_blue(),
            (network_index + 1).to_string().bright_green(),
            total_networks.to_string().bright_green(),
            rpc_url.bright_white()
        );
    } else {
        println!(
            "\n{} {}",
            "🌐 Deploying to:".bright_blue().bold(),
            rpc_url.bright_white()
        );
    }
    
    println!(
        "{}",
        "📦 All public keys for this deployment:"
            .bright_yellow()
            .bold()
    );

    println!(
        "  {}: {}",
        "Create key".cyan(),
        create_key.to_string().bright_white()
    );
    println!(
        "  {}: {}",
        "Contributor".cyan(),
        contributor_pubkey.to_string().bright_white()
    );
    println!(
        "  {}: {}",
        "Multisig PDA".cyan(),
        multisig_address.to_string().bright_green()
    );
    println!(
        "  {}: {}",
        "Vault PDA (index 0)".cyan(),
        vault_address.to_string().bright_green()
    );
    
    for (i, member) in members.iter().enumerate() {
        let perms = decode_permissions(member.permissions.mask);
        let label = if member.key == *contributor_pubkey {
            "Contributor".to_string()
        } else {
            format!("Member {}", i)
        };
        println!(
            "  {}: {} ({})",
            label.cyan(),
            member.key.to_string().bright_white(),
            perms.join(", ").bright_cyan()
        );
    }
    println!();
}

// Transaction creation functions
pub fn create_feature_activation_transaction_message() -> TransactionMessage {
    use crate::squads::SmallVec;
    
    // Create feature activation instructions for a test feature
    let feature_id = Pubkey::new_unique();
    let funding_address = Pubkey::new_unique();
    let instructions = create_feature_activation(&feature_id, &funding_address);
    
    // Build account keys list for the message
    let mut account_keys = vec![
        funding_address,                                      // 0: Funding address (signer, writable)
        feature_id,                                          // 1: Feature account (writable)
        solana_system_interface::program::ID,                // 2: System program
        crate::feature_gate_program::FEATURE_GATE_PROGRAM_ID, // 3: Feature gate program
    ];
    
    // Compile instructions into CompiledInstructions with SmallVec
    let mut compiled_instructions = Vec::new();
    
    for instruction in instructions {
        // Find program_id index in account_keys
        let program_id_index = account_keys.iter()
            .position(|key| *key == instruction.program_id)
            .unwrap_or_else(|| {
                account_keys.push(instruction.program_id);
                account_keys.len() - 1
            }) as u8;
            
        // Map account pubkeys to indices
        let account_indexes: Vec<u8> = instruction.accounts.iter()
            .map(|account_meta| {
                account_keys.iter()
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
        num_signers: 1,              // funding_address is the signer
        num_writable_signers: 1,     // funding_address is writable signer
        num_writable_non_signers: 1, // feature_id is writable non-signer
        account_keys: SmallVec::from(account_keys),
        instructions: SmallVec::from(compiled_instructions),
        address_table_lookups: SmallVec::from(vec![]),
    }
}

pub fn create_feature_revocation_transaction_message() -> TransactionMessage {
    use crate::squads::SmallVec;
    
    // Create feature revocation instruction for a test feature
    let feature_id = Pubkey::new_unique();
    let instruction = crate::feature_gate_program::revoke_pending_activation(&feature_id);
    
    // Build account keys list for the message
    let mut account_keys = vec![
        feature_id,                                          // 0: Feature account (signer, writable)
        crate::feature_gate_program::INCINERATOR_ID,         // 1: Incinerator (writable)
        solana_system_interface::program::ID,                // 2: System program
        crate::feature_gate_program::FEATURE_GATE_PROGRAM_ID, // 3: Feature gate program
    ];
    
    // Find program_id index in account_keys
    let program_id_index = account_keys.iter()
        .position(|key| *key == instruction.program_id)
        .unwrap_or_else(|| {
            account_keys.push(instruction.program_id);
            account_keys.len() - 1
        }) as u8;
        
    // Map account pubkeys to indices
    let account_indexes: Vec<u8> = instruction.accounts.iter()
        .map(|account_meta| {
            account_keys.iter()
                .position(|key| *key == account_meta.pubkey)
                .unwrap_or_else(|| {
                    account_keys.push(account_meta.pubkey);
                    account_keys.len() - 1
                }) as u8
        })
        .collect();
        
    let compiled_instructions = vec![CompiledInstruction {
        program_id_index,
        account_indexes: SmallVec::from(account_indexes),
        data: SmallVec::from(instruction.data),
    }];
    
    TransactionMessage {
        num_signers: 1,              // feature_id is the signer
        num_writable_signers: 1,     // feature_id is writable signer
        num_writable_non_signers: 1, // incinerator is writable non-signer
        account_keys: SmallVec::from(account_keys),
        instructions: SmallVec::from(compiled_instructions),
        address_table_lookups: SmallVec::from(vec![]),
    }
}

pub async fn create_and_send_transaction_proposal(
    rpc_url: &str,
    fee_payer_keypair: &Option<Keypair>,
    contributor_keypair: &Keypair,
    multisig_address: &Pubkey,
    transaction_type: &str,
    transaction_index: u64,
) -> Result<()> {
    println!(
        "\n{} Creating transaction and proposal for multisig governance ({})...",
        "📋".bright_blue(),
        transaction_type.bright_cyan()
    );

    let transaction_message = match transaction_type {
        "activation" => create_feature_activation_transaction_message(),
        "revocation" => create_feature_revocation_transaction_message(),
        _ => return Err(anyhow::anyhow!("Invalid transaction type: {}", transaction_type)),
    };
    
    let rpc_client = RpcClient::new(rpc_url);
    let recent_blockhash = rpc_client.get_latest_blockhash()
        .map_err(|e| anyhow::anyhow!("Failed to get recent blockhash: {}", e))?;

    let fee_payer_pubkey = fee_payer_keypair
        .as_ref()
        .map(|kp| kp.pubkey())
        .unwrap_or_else(|| contributor_keypair.pubkey());

    let (message, transaction_pda, proposal_pda) = create_transaction_and_proposal_message(
        None, // Use default program ID
        &fee_payer_pubkey,
        &contributor_keypair.pubkey(),
        multisig_address,
        transaction_index,
        0, // Vault index 0 (default vault for feature gates)
        transaction_message,
        Some(5000), // Priority fee
        Some(300_000), // Compute unit limit
        recent_blockhash,
    ).map_err(|e| anyhow::anyhow!("Failed to create transaction and proposal message: {}", e))?;

    println!(
        "  {}: {}",
        "Transaction PDA".cyan(),
        transaction_pda.to_string().bright_green()
    );
    println!(
        "  {}: {}",
        "Proposal PDA".cyan(),
        proposal_pda.to_string().bright_green()
    );
    println!(
        "  {}: {} instructions",
        "Transaction Instructions".cyan(),
        "4".bright_white() // set_compute_unit_price, set_compute_unit_limit, create_transaction, create_proposal
    );

    let signers: &[&dyn Signer] = if fee_payer_pubkey == contributor_keypair.pubkey() {
        &[contributor_keypair]
    } else {
        &[
            fee_payer_keypair.as_ref().unwrap() as &dyn Signer,
            contributor_keypair as &dyn Signer
        ]
    };

    let transaction = VersionedTransaction::try_new(VersionedMessage::V0(message), signers)
        .map_err(|e| anyhow::anyhow!("Failed to create signed transaction: {}", e))?;

    // Display the transaction signature before sending
    let expected_signature = transaction.signatures[0];
    println!(
        "  {}: {}",
        "Transaction Signature (before send)".cyan(),
        expected_signature.to_string().bright_white()
    );
    
    println!("{} Sending transaction to RPC...", "📤".bright_blue());

    let signature = rpc_client.send_and_confirm_transaction(&transaction)
        .map_err(|e| anyhow::anyhow!("Failed to send transaction and proposal: {}", e))?;

    println!(
        "{} Transaction and proposal created successfully!",
        "✅".bright_green()
    );
    println!(
        "  {}: {}",
        "Confirmed Transaction Signature".cyan(),
        signature.to_string().bright_white()
    );

    Ok(())
}

// Validation functions
pub fn validate_pubkey_with_retry(prompt: &str) -> Result<Pubkey> {
    loop {
        let input = Text::new(prompt).prompt()?;
        match Pubkey::from_str(&input.trim()) {
            Ok(pubkey) => {
                println!(
                    "  {} Valid public key: {}",
                    "✓".bright_green(),
                    pubkey.to_string().bright_white()
                );
                return Ok(pubkey);
            }
            Err(_) => {
                println!(
                    "  {} Invalid public key format. Please try again.",
                    "❌".bright_red()
                );
                println!(
                    "  {} Public keys should be valid base58-encoded addresses",
                    "💡".bright_blue()
                );

                let retry = Confirm::new("Try again?").with_default(true).prompt()?;
                if !retry {
                    return Err(anyhow::anyhow!("User cancelled public key entry"));
                }
            }
        }
    }
}

pub fn validate_threshold(input: &str, max_members: usize, default: u16) -> Result<u16> {
    if input.trim().is_empty() {
        return Ok(default);
    }

    match input.trim().parse::<u16>() {
        Ok(threshold) if threshold == 0 => Err(anyhow::anyhow!("Threshold must be at least 1")),
        Ok(threshold) if threshold > max_members as u16 => Err(anyhow::anyhow!(
            "Threshold cannot exceed number of members ({})",
            max_members
        )),
        Ok(threshold) => {
            println!(
                "  {} Valid threshold: {}",
                "✓".bright_green(),
                threshold.to_string().bright_white()
            );
            Ok(threshold)
        }
        Err(_) => Err(anyhow::anyhow!(
            "Invalid number format. Please enter a positive integer."
        )),
    }
}

pub fn validate_rpc_url(url: &str) -> Result<String> {
    let url = url.trim();

    if url.is_empty() {
        return Err(anyhow::anyhow!("URL cannot be empty"));
    }

    // Check if it's a valid URL
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(anyhow::anyhow!("URL must start with http:// or https://"));
    }

    // Basic URL validation - check for valid characters and structure
    if !url.contains("://") {
        return Err(anyhow::anyhow!("Invalid URL format"));
    }

    // Check for common Solana RPC patterns
    if url.contains("solana.com")
        || url.contains("localhost")
        || url.contains("127.0.0.1")
        || url.contains("rpc")
    {
        println!("  {} Valid RPC URL format detected", "✓".bright_green());
    } else {
        println!(
            "  {} Warning: URL doesn't match common Solana RPC patterns",
            "⚠️".bright_yellow()
        );
        let confirm = Confirm::new("Continue with this URL?")
            .with_default(false)
            .prompt()?;
        if !confirm {
            return Err(anyhow::anyhow!("User cancelled due to unusual URL"));
        }
    }

    Ok(url.to_string())
}

pub fn choose_network_mode(config: &Config, use_saved_config: bool) -> Result<(bool, Vec<String>)> {
    if !use_saved_config {
        return Ok((false, Vec::new()));
    }

    let available_networks = if !config.networks.is_empty() {
        config.networks.clone()
    } else {
        vec![config.network.clone()]
    };

    if available_networks.is_empty() {
        return Ok((false, Vec::new()));
    }

    println!(
        "\n{}",
        "🌐 Network Deployment Options:".bright_blue().bold()
    );
    println!("  Option 1: Deploy to all saved networks automatically");
    for (i, network) in available_networks.iter().enumerate() {
        let network_name = if network.contains("devnet") {
            "Devnet"
        } else if network.contains("testnet") {
            "Testnet"
        } else if network.contains("mainnet") {
            "Mainnet"
        } else {
            "Custom"
        };
        println!(
            "    {}: {} ({})",
            format!("Network {}", i + 1).cyan(),
            network_name.bright_white(),
            network.bright_white()
        );
    }
    println!("  Option 2: Manual network entry (prompt for each deployment)");

    let use_saved_networks = Confirm::new("Use saved networks for deployment?")
        .with_default(true)
        .prompt()?;

    Ok((use_saved_networks, available_networks))
}

pub fn review_config(config: &Config) -> Result<bool> {
    if config.members.is_empty() && config.networks.is_empty() {
        return Ok(false);
    }

    println!(
        "\n{}",
        "📋 Found existing configuration:".bright_yellow().bold()
    );
    println!(
        "  {}: {}",
        "Threshold".cyan(),
        config.threshold.to_string().bright_green()
    );

    // Show fee payer path
    if let Some(fee_payer_path) = &config.fee_payer_path {
        println!(
            "  {}: {}",
            "Fee payer keypair".cyan(),
            fee_payer_path.bright_green()
        );
    } else {
        println!(
            "  {}: {}",
            "Fee payer keypair".cyan(),
            "Not configured".bright_yellow()
        );
    }

    // Show networks
    let networks_to_show = if !config.networks.is_empty() {
        &config.networks
    } else {
        &vec![config.network.clone()]
    };

    println!(
        "  {}: {} networks",
        "Saved networks".cyan(),
        networks_to_show.len().to_string().bright_green()
    );
    for (i, network) in networks_to_show.iter().enumerate() {
        println!(
            "    {}: {}",
            format!("Network {}", i + 1).cyan(),
            network.bright_white()
        );
    }

    if !config.members.is_empty() {
        println!(
            "  {}: {} members",
            "Saved members".cyan(),
            config.members.len().to_string().bright_green()
        );
        for (i, member) in config.members.iter().enumerate() {
            println!(
                "    {}: {}",
                format!("Member {}", i + 1).cyan(),
                member.bright_white()
            );
        }
    }

    println!();
    let use_config = Confirm::new("Use these saved members and settings?")
        .with_default(true)
        .prompt()?;

    Ok(use_config)
}

pub fn decode_permissions(mask: u8) -> Vec<String> {
    let mut permissions = Vec::new();
    if mask & 1 != 0 {
        permissions.push("Initiate".to_string());
    }
    if mask & 2 != 0 {
        permissions.push("Vote".to_string());
    }
    if mask & 4 != 0 {
        permissions.push("Execute".to_string());
    }
    permissions
}