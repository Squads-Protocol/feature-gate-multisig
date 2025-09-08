mod provision;
mod squads;
mod feature_gate_program;

use crate::provision::create_multisig;
use crate::squads::{Member, Permissions, Multisig, get_vault_pda};
use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use inquire::{Select, Text, Confirm};
use serde::{Deserialize, Serialize};

use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::{Signer, EncodableKey};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use tabled::{Table, Tabled, settings::Style};

#[derive(Parser)]
#[command(name = "feature-gate-multisig-tool")]
#[command(about = "A command-line tool for rapidly provisioning minimal Squads multisig setups specifically designed for Solana feature gate governance")]
#[command(long_about = "This tool enables parties to create multisig wallets where the default vault address can be mapped to feature gate account addresses, allowing collective voting on whether new Solana features should be implemented.

Features:
• 🚀 Rapid provisioning of Squads multisig wallets
• 🌐 Multi-network deployment (automatic or manual)
• 👥 Interactive member management with permission assignment
• 📋 Persistent configuration with saved networks and members
• 🎨 Rich CLI experience with colored output and tables

For more information, run: feature-gate-multisig-tool help <COMMAND>")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Create a new multisig wallet with interactive setup")]
    #[command(long_about = "Creates a new multisig wallet for feature gate governance. The tool will guide you through:
• Configuration review (if saved config exists)
• Member collection (interactive or from saved config)
• Network deployment selection (automatic or manual)
• Multi-network deployment with consistent addresses
• Comprehensive deployment summary

The contributor key receives Initiate-only permissions, while additional members receive full permissions (Initiate, Vote, Execute).")]
    Create {
        #[arg(short, long, help = "Number of required signatures (will be prompted if not provided)")]
        threshold: Option<u32>,
        #[arg(short, long, help = "Signers (currently unused - members are collected interactively)")]
        signers: Option<Vec<String>>,
        #[arg(short = 'k', long, help = "Keypair file path for paying transaction fees")]
        keypair: Option<String>,
    },
    #[command(about = "Show feature multisig details for a given address")]
    #[command(long_about = "Display detailed information about an existing multisig wallet including member permissions, threshold settings, and network deployment status.")]
    Show {
        #[arg(help = "The multisig address to inspect")]
        address: Option<String>,
    },
    #[command(about = "Start interactive mode (default when no command is specified)")]
    #[command(long_about = "Launches the interactive mode which provides a guided experience for creating multisig wallets. This is the default mode when no command is specified.")]
    Interactive,
    #[command(about = "Show current configuration including networks and saved members")]
    #[command(long_about = "Displays the current configuration stored in ~/.feature-gate-multisig-tool/config.json including:
• Saved networks array for automatic deployment
• Saved member public keys
• Default threshold setting
• Configuration file location")]
    Config,
}

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

fn get_config_path() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    Ok(home_dir
        .join(".feature-gate-multisig-tool")
        .join("config.json"))
}

fn load_config() -> Result<Config> {
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

fn save_config(config: &Config) -> Result<()> {
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Some(command) => handle_command(command).await,
        None => interactive_mode().await,
    };

    if let Err(e) = result {
        eprintln!("{} {}", "❌ Error:".bright_red().bold(), e.to_string().bright_red());

        // Provide helpful error messages for common issues
        let error_msg = e.to_string();
        if error_msg.contains("config") {
            eprintln!("{} Try running: {}", "💡 Hint:".bright_blue(), "feature-gate-multisig-tool config".bright_cyan());
        } else if error_msg.contains("address") || error_msg.contains("pubkey") {
            eprintln!("{} Public keys should be valid base58-encoded addresses", "💡 Hint:".bright_blue());
        } else if error_msg.contains("network") || error_msg.contains("URL") {
            eprintln!("{} Network URLs should start with https:// (e.g., https://api.devnet.solana.com)", "💡 Hint:".bright_blue());
        } else if error_msg.contains("threshold") {
            eprintln!("{} Threshold must be a positive number not exceeding member count", "💡 Hint:".bright_blue());
        }

        eprintln!("\n{} Run {} for usage information", "💡".bright_blue(), "--help".bright_cyan());
        std::process::exit(1);
    }
}

async fn handle_command(command: Commands) -> Result<()> {
    let mut config = load_config()?;

    match command {
        Commands::Create { threshold, signers: _, keypair } => {
            let threshold = if let Some(t) = threshold {
                if t == 0 {
                    println!("{} Threshold cannot be 0, using default: {}", "⚠️".bright_yellow(), config.threshold);
                    config.threshold
                } else {
                    t as u16
                }
            } else {
                loop {
                    let input = Text::new(&format!(
                        "Enter threshold (required signatures) [{}]:",
                        config.threshold
                    ))
                    .prompt()
                    .unwrap_or_default();

                    // For now, use a reasonable max (we'll adjust this later based on actual member count)
                    match validate_threshold(&input, 10, config.threshold) {
                        Ok(t) => break t,
                        Err(e) => {
                            println!("  {} {}", "❌".bright_red(), e.to_string().bright_red());
                            continue;
                        }
                    }
                }
            };

            create_feature_multisig(&mut config, threshold, vec![], keypair).await
        }
        Commands::Show { address } => {
            let address = if let Some(addr) = address {
                // Validate provided address
                match Pubkey::from_str(&addr) {
                    Ok(_) => addr,
                    Err(_) => {
                        println!("{} Invalid address format: {}", "❌".bright_red(), addr.bright_red());
                        return Err(anyhow::anyhow!("Invalid multisig address format"));
                    }
                }
            } else {
                validate_pubkey_with_retry("Enter multisig address:")?.to_string()
            };
            show_multisig(&config, &address).await
        }
        Commands::Interactive => interactive_mode().await,
        Commands::Config => show_config(&config).await,
    }
}

async fn interactive_mode() -> Result<()> {
    let mut config = load_config()?;

    loop {
        let options = vec![
            "Create new feature gate multisig",
            "Show feature gate multisig details",
            "Show configuration",
            "Exit",
        ];

        let choice = Select::new("What would you like to do?", options).prompt()?;

        match choice {
            "Create new feature gate multisig" => {
                let threshold: u16 = Text::new(&format!(
                    "Enter threshold (required signatures) [{}]:",
                    config.threshold
                ))
                .prompt()?
                .parse()
                .unwrap_or(config.threshold);

                create_feature_multisig(&mut config, threshold, vec![], None).await?;
            }
            "Show feature gate multisig details" => {
                let address = Text::new("Enter the main multisig address:").prompt()?;
                show_multisig(&config, &address).await?;
            }
            "Show configuration" => {
                show_config(&config).await?;
            }
            "Exit" => break,
            _ => unreachable!(),
        }

        println!("\n");
    }

    Ok(())
}

async fn create_feature_multisig(
    config: &mut Config,
    threshold: u16,
    _sub_multisigs: Vec<String>,
    keypair_path: Option<String>,
) -> Result<()> {
    println!("{}", "🚀 Creating feature gate multisig configuration".bright_cyan().bold());

    // Check for existing configuration and ask user
    let use_saved_config = review_config(config)?;

    let (final_threshold, mut members) = if use_saved_config {
        // Use saved configuration
        println!("{} Using saved configuration", "✅".bright_green());

        // Parse saved members
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
                    println!("  {} Invalid saved member key: {}, skipping...",
                             "⚠️".bright_yellow(), member_str);
                }
            }
        }

        (config.threshold, parsed_members)
    } else {
        // Collect members interactively
        println!("{} Collecting members interactively", "🔄".bright_cyan());

        let mut interactive_members = Vec::new();

        // Collect additional member keys
        loop {
            let add_member = Confirm::new("Add a member?")
                .with_default(true)
                .prompt()?;

            if !add_member {
                break;
            }

            match validate_pubkey_with_retry("Enter member public key:") {
                Ok(member_key) => {
                    interactive_members.push(Member {
                        key: member_key,
                        permissions: Permissions { mask: 7 },
                    });
                    println!("  {} Added member: {} ({})",
                             "✓".bright_green(),
                             member_key.to_string().bright_white(),
                             "Initiate, Vote, Execute".bright_cyan());
                }
                Err(e) => {
                    println!("  {} Failed to add member: {}", "❌".bright_red(), e.to_string().bright_red());
                    continue;
                }
            }
        }

        (threshold, interactive_members)
    };

    // Load fee payer keypair from CLI arg or config
    let fee_payer_keypair = if let Some(path) = keypair_path {
        println!("{} Loading fee payer keypair from CLI: {}", "💰".bright_blue(), path.bright_white());
        Some(Keypair::read_from_file(&path)
            .map_err(|e| anyhow::anyhow!("Failed to load keypair from {}: {}", path, e))?)
    } else if let Some(path) = &config.fee_payer_path {
        println!("{} Loading fee payer keypair from config: {}", "💰".bright_blue(), path.bright_white());
        Some(Keypair::read_from_file(path)
            .map_err(|e| anyhow::anyhow!("Failed to load keypair from config path {}: {}", path, e))?)
    } else {
        println!("{} No fee payer keypair provided", "⚠️".bright_yellow());
        None
    };

    // Create contributor keypair (always separate from fee payer)
    let contributor_initiator_keypair = Keypair::new();
    let contributor_pubkey = contributor_initiator_keypair.pubkey();

    // Create a persistent create_key for all deployments
    let create_key = Keypair::new();

    // Add contributor as a member with permission 1 (bitmask for Initiate only)
    members.insert(0, Member {
        key: contributor_pubkey,
        permissions: Permissions { mask: 1 },
    });

    println!("\n{}", "📋 Final Configuration:".bright_yellow().bold());
    println!("  {}: {}", "Contributor public key".cyan(), contributor_pubkey.to_string().bright_white());
    println!("  {}: {}", "Create key".cyan(), create_key.pubkey().to_string().bright_white());
    if let Some(fee_payer) = &fee_payer_keypair {
        println!("  {}: {}", "Fee payer".cyan(), fee_payer.pubkey().to_string().bright_green());
    } else {
        println!("  {}: {} (same as contributor)", "Fee payer".cyan(), contributor_pubkey.to_string().bright_yellow());
    }
    println!("  {}: {}", "Threshold".cyan(), final_threshold.to_string().bright_green());

    println!("\n{}", "👥 All Members:".bright_yellow().bold());
    println!("  {} Contributor: {} ({})",
             "✓".bright_green(),
             contributor_pubkey.to_string().bright_white(),
             "Initiate".bright_cyan());

    // Display all other members
    for (i, member) in members.iter().skip(1).enumerate() {
        let perms = decode_permissions(member.permissions.mask);
        println!("  {} Member {}: {} ({})",
                 "✓".bright_green(),
                 i + 1,
                 member.key.to_string().bright_white(),
                 perms.join(", ").bright_cyan());
    }

    println!("\n{} {}",
             "📊 Total members:".bright_yellow().bold(),
             members.len().to_string().bright_green());
    println!();

    // Store deployment results
    let mut deployments = Vec::new();

    // Determine network deployment mode
    let (use_saved_networks, saved_networks) = choose_network_mode(config, use_saved_config)?;

    if use_saved_networks && !saved_networks.is_empty() {
        // Deploy to all saved networks automatically
        println!("\n{} Deploying to all saved networks", "🚀".bright_cyan());

        for (i, rpc_url) in saved_networks.iter().enumerate() {
            println!("\n{} Deployment {} of {} to: {}",
                     "📡".bright_blue(),
                     (i + 1).to_string().bright_green(),
                     saved_networks.len().to_string().bright_green(),
                     rpc_url.bright_white());
            println!("{}", "📦 All public keys for this deployment:".bright_yellow().bold());

            // Derive addresses that will be created
            let multisig_address = crate::squads::get_multisig_pda(&create_key.pubkey(), None).0;
            let vault_address = get_vault_pda(&multisig_address, 0, None).0;

            println!("  {}: {}", "Create key".cyan(), create_key.pubkey().to_string().bright_white());
            println!("  {}: {}", "Contributor".cyan(), contributor_pubkey.to_string().bright_white());
            println!("  {}: {}", "Multisig PDA".cyan(), multisig_address.to_string().bright_green());
            println!("  {}: {}", "Vault PDA (index 0)".cyan(), vault_address.to_string().bright_green());
            for (i, member) in members.iter().enumerate() {
                let perms = decode_permissions(member.permissions.mask);
                let label = if member.key == contributor_pubkey {
                    "Contributor".to_string()
                } else {
                    format!("Member {}", i)
                };
                println!("  {}: {} ({})",
                         label.cyan(),
                         member.key.to_string().bright_white(),
                         perms.join(", ").bright_cyan());
            }
            println!();

            // Create multisig using provision code
            // Use fee payer if provided, otherwise use contributor keypair
            let signer_for_creation = fee_payer_keypair.as_ref()
                .map(|kp| kp as &dyn Signer)
                .unwrap_or(&contributor_initiator_keypair as &dyn Signer);

            match create_multisig(
                rpc_url.clone(),
                None, // Use default program ID
                signer_for_creation,
                &create_key,
                None, // No config authority
                members.clone(),
                final_threshold,
                None, // No rent collector
                Some(5000), // Priority fee
            ).await {
                Ok((multisig_address, signature)) => {
                    let vault_address = get_vault_pda(&multisig_address, 0, None).0;

                    deployments.push(DeploymentResult {
                        rpc_url: rpc_url.clone(),
                        multisig_address,
                        vault_address,
                        transaction_signature: signature,
                    });

                    println!("{} Deployment successful on {}",
                             "✅".bright_green(),
                             rpc_url.bright_white());
                }
                Err(e) => {
                    println!("{} Failed to deploy on {}: {}",
                             "❌".bright_red(),
                             rpc_url.bright_white(),
                             e.to_string().red());
                }
            }

            // Add delay between deployments if there are more networks
            if i < saved_networks.len() - 1 {
                println!("\n{} Proceeding to next network...", "⏳".bright_yellow());
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    } else {
        // Manual network entry mode - existing loop logic
        println!("\n{} Manual network entry mode", "🔄".bright_cyan());

        loop {
            let default_network = if !config.networks.is_empty() {
                &config.networks[0]
            } else {
                &config.network
            };

            let rpc_url = loop {
                let input = Text::new("Enter RPC URL for deployment:")
                    .with_default(default_network)
                    .prompt()?;

                match validate_rpc_url(&input) {
                    Ok(url) => break url,
                    Err(e) => {
                        println!("  {} {}", "❌".bright_red(), e.to_string().bright_red());
                        let retry = Confirm::new("Try again?")
                            .with_default(true)
                            .prompt()?;
                        if !retry {
                            return Err(anyhow::anyhow!("User cancelled network entry"));
                        }
                    }
                }
            };

        println!("\n{} {}", "🌐 Deploying to:".bright_blue().bold(), rpc_url.bright_white());
        println!("{}", "📦 All public keys for this deployment:".bright_yellow().bold());

        // Derive addresses that will be created
        let multisig_address = crate::squads::get_multisig_pda(&create_key.pubkey(), None).0;
        let vault_address = get_vault_pda(&multisig_address, 0, None).0;

        println!("  {}: {}", "Create key".cyan(), create_key.pubkey().to_string().bright_white());
        println!("  {}: {}", "Contributor".cyan(), contributor_pubkey.to_string().bright_white());
        println!("  {}: {}", "Multisig PDA".cyan(), multisig_address.to_string().bright_green());
        println!("  {}: {}", "Vault PDA (index 0)".cyan(), vault_address.to_string().bright_green());
        for (i, member) in members.iter().enumerate() {
            let perms = decode_permissions(member.permissions.mask);
            let label = if member.key == contributor_pubkey {
                "Contributor".to_string()
            } else {
                format!("Member {}", i)
            };
            println!("  {}: {} ({})",
                     label.cyan(),
                     member.key.to_string().bright_white(),
                     perms.join(", ").bright_cyan());
        }
        println!();

        // Create multisig using provision code
        // Use fee payer if provided, otherwise use contributor keypair
        let signer_for_creation = fee_payer_keypair.as_ref()
            .map(|kp| kp as &dyn Signer)
            .unwrap_or(&contributor_initiator_keypair as &dyn Signer);

        match create_multisig(
            rpc_url.clone(),
            None, // Use default program ID
            signer_for_creation,
            &create_key,
            None, // No config authority
            members.clone(),
            final_threshold,
            None, // No rent collector
            Some(5000), // Priority fee
        ).await {
            Ok((multisig_address, signature)) => {
                let vault_address = get_vault_pda(&multisig_address, 0, None).0;

                deployments.push(DeploymentResult {
                    rpc_url: rpc_url.clone(),
                    multisig_address,
                    vault_address,
                    transaction_signature: signature,
                });

                println!("{} Deployment successful on {}",
                         "✅".bright_green(),
                         rpc_url.bright_white());
            }
            Err(e) => {
                println!("{} Failed to deploy on {}: {}",
                         "❌".bright_red(),
                         rpc_url.bright_white(),
                         e.to_string().red());
            }
        }

            // Ask if they want to deploy to another network
            let deploy_another = Confirm::new("Deploy to another network with the same configuration?")
                .with_default(false)
                .prompt()?;

            if !deploy_another {
                break;
            }

            println!();
        }
    }

    // Print summary table
    print_deployment_summary(&deployments, &members, final_threshold, &create_key.pubkey());

    // Save updated configuration (excluding contributor key)
    if !deployments.is_empty() {
        // Update config with current settings (excluding contributor)
        config.threshold = final_threshold;
        config.members = members
            .iter()
            .skip(1) // Skip contributor (index 0)
            .map(|member| member.key.to_string())
            .collect();

        save_config(config)?;
        println!("\n{} Configuration saved for future use", "💾".bright_green());
    }

    Ok(())
}

async fn show_multisig(config: &Config, address: &str) -> Result<()> {
    // Parse the multisig address
    let multisig_pubkey = Pubkey::from_str(address)
        .map_err(|_| anyhow::anyhow!("Invalid multisig address format"))?;

    println!("{}", "🔍 Fetching multisig details...".bright_yellow().bold());
    println!();

    // Determine which network to use
    let rpc_url = if !config.networks.is_empty() {
        // If we have multiple networks, ask which one to query
        if config.networks.len() > 1 {
            println!("Available networks:");
            for (i, network) in config.networks.iter().enumerate() {
                println!("  {}: {}", i + 1, network);
            }

            // For testing, let's try mainnet first if available, otherwise use the first network
            if config.networks.iter().any(|n| n.contains("mainnet")) {
                let mainnet_url = config.networks.iter().find(|n| n.contains("mainnet")).unwrap();
                println!("🌐 Trying mainnet-beta first: {}", mainnet_url);
                mainnet_url.clone()
            } else {
                config.networks[0].clone()
            }
        } else {
            config.networks[0].clone()
        }
    } else if !config.network.is_empty() {
        config.network.clone()
    } else {
        "https://api.devnet.solana.com".to_string()
    };

    println!("📡 Querying network: {}", rpc_url.bright_white());
    println!("🎯 Multisig address: {}", multisig_pubkey.to_string().bright_white());
    println!();

    // Create RPC client and fetch account data directly
    let rpc_client = RpcClient::new(&rpc_url);

    // Use get_account_data to get just the data
    let account_data = rpc_client.get_account_data(&multisig_pubkey)
        .map_err(|e| {
            let error_str = e.to_string();
            if error_str.contains("AccountNotFound") || error_str.contains("could not find account") {
                anyhow::anyhow!("Account not found: {}. This address may not exist on the selected network or may not be a multisig account.", multisig_pubkey)
            } else {
                anyhow::anyhow!("Failed to fetch multisig account data: {}", e)
            }
        })?;

    if account_data.len() < 8 {
        return Err(anyhow::anyhow!("Account data too small to be a valid multisig"));
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
            value: multisig.rent_collector
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

    println!("{} ({} total)", "👥 MEMBERS".bright_blue().bold(), multisig.members.len());
    println!();

    let member_data: Vec<MemberInfo> = multisig.members.iter().enumerate().map(|(i, member)| {
        let perms = decode_permissions(member.permissions.mask);
        MemberInfo {
            index: i + 1,
            pubkey: member.key.to_string(),
            permissions: if perms.is_empty() { "None".to_string() } else { perms.join(", ") },
            bitmask: member.permissions.mask,
        }
    }).collect();

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

    println!("{}", "✅ Multisig details retrieved successfully!".bright_green());

    Ok(())
}

async fn show_config(config: &Config) -> Result<()> {
    println!("{}", "📋 Configuration:".bright_yellow().bold());
    println!("  {}: {:?}", "Config file".cyan(), get_config_path()?.to_string_lossy().bright_white());
    println!("  {}: {}", "Default threshold".cyan(), config.threshold.to_string().bright_green());

    // Display fee payer path
    if let Some(fee_payer_path) = &config.fee_payer_path {
        println!("  {}: {}", "Fee payer keypair".cyan(), fee_payer_path.bright_green());
    } else {
        println!("  {}: {}", "Fee payer keypair".cyan(), "Not configured".bright_yellow());
    }

    // Display networks array if available, otherwise show legacy single network
    if !config.networks.is_empty() {
        println!("  {}: {} networks", "Saved networks".cyan(), config.networks.len().to_string().bright_green());
        for (i, network) in config.networks.iter().enumerate() {
            println!("    {}: {}", format!("Network {}", i + 1).cyan(), network.bright_white());
        }
    } else if !config.network.is_empty() {
        println!("  {}: {}", "Default network".cyan(), config.network.bright_white());
    }

    println!("  {}: {} members", "Saved members".cyan(), config.members.len().to_string().bright_green());

    if !config.members.is_empty() {
        for (i, member) in config.members.iter().enumerate() {
            println!("    {}: {}", format!("Member {}", i + 1).cyan(), member.bright_white());
        }
    }

    Ok(())
}

fn decode_permissions(mask: u8) -> Vec<String> {
    let mut permissions = Vec::new();
    if mask & 1 != 0 { permissions.push("Initiate".to_string()); }
    if mask & 2 != 0 { permissions.push("Vote".to_string()); }
    if mask & 4 != 0 { permissions.push("Execute".to_string()); }
    permissions
}

fn validate_rpc_url(url: &str) -> Result<String> {
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
    if url.contains("solana.com") || url.contains("localhost") || url.contains("127.0.0.1") || url.contains("rpc") {
        println!("  {} Valid RPC URL format detected", "✓".bright_green());
    } else {
        println!("  {} Warning: URL doesn't match common Solana RPC patterns", "⚠️".bright_yellow());
        let confirm = Confirm::new("Continue with this URL?")
            .with_default(false)
            .prompt()?;
        if !confirm {
            return Err(anyhow::anyhow!("User cancelled due to unusual URL"));
        }
    }

    Ok(url.to_string())
}

fn validate_pubkey_with_retry(prompt: &str) -> Result<Pubkey> {
    loop {
        let input = Text::new(prompt).prompt()?;
        match Pubkey::from_str(&input.trim()) {
            Ok(pubkey) => {
                println!("  {} Valid public key: {}", "✓".bright_green(), pubkey.to_string().bright_white());
                return Ok(pubkey);
            }
            Err(_) => {
                println!("  {} Invalid public key format. Please try again.", "❌".bright_red());
                println!("  {} Public keys should be valid base58-encoded addresses", "💡".bright_blue());

                let retry = Confirm::new("Try again?")
                    .with_default(true)
                    .prompt()?;
                if !retry {
                    return Err(anyhow::anyhow!("User cancelled public key entry"));
                }
            }
        }
    }
}

fn validate_threshold(input: &str, max_members: usize, default: u16) -> Result<u16> {
    if input.trim().is_empty() {
        return Ok(default);
    }

    match input.trim().parse::<u16>() {
        Ok(threshold) if threshold == 0 => {
            Err(anyhow::anyhow!("Threshold must be at least 1"))
        }
        Ok(threshold) if threshold > max_members as u16 => {
            Err(anyhow::anyhow!("Threshold cannot exceed number of members ({})", max_members))
        }
        Ok(threshold) => {
            println!("  {} Valid threshold: {}", "✓".bright_green(), threshold.to_string().bright_white());
            Ok(threshold)
        }
        Err(_) => {
            Err(anyhow::anyhow!("Invalid number format. Please enter a positive integer."))
        }
    }
}

fn choose_network_mode(config: &Config, use_saved_config: bool) -> Result<(bool, Vec<String>)> {
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

    println!("\n{}", "🌐 Network Deployment Options:".bright_blue().bold());
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
        println!("    {}: {} ({})", format!("Network {}", i + 1).cyan(), network_name.bright_white(), network.bright_white());
    }
    println!("  Option 2: Manual network entry (prompt for each deployment)");

    let use_saved_networks = Confirm::new("Use saved networks for deployment?")
        .with_default(true)
        .prompt()?;

    Ok((use_saved_networks, available_networks))
}

fn review_config(config: &Config) -> Result<bool> {
    if config.members.is_empty() && config.networks.is_empty() {
        return Ok(false);
    }

    println!("\n{}", "📋 Found existing configuration:".bright_yellow().bold());
    println!("  {}: {}", "Threshold".cyan(), config.threshold.to_string().bright_green());

    // Show fee payer path
    if let Some(fee_payer_path) = &config.fee_payer_path {
        println!("  {}: {}", "Fee payer keypair".cyan(), fee_payer_path.bright_green());
    } else {
        println!("  {}: {}", "Fee payer keypair".cyan(), "Not configured".bright_yellow());
    }

    // Show networks
    let networks_to_show = if !config.networks.is_empty() {
        &config.networks
    } else {
        &vec![config.network.clone()]
    };

    println!("  {}: {} networks", "Saved networks".cyan(), networks_to_show.len().to_string().bright_green());
    for (i, network) in networks_to_show.iter().enumerate() {
        println!("    {}: {}", format!("Network {}", i + 1).cyan(), network.bright_white());
    }

    if !config.members.is_empty() {
        println!("  {}: {} members", "Saved members".cyan(), config.members.len().to_string().bright_green());
        for (i, member) in config.members.iter().enumerate() {
            println!("    {}: {}", format!("Member {}", i + 1).cyan(), member.bright_white());
        }
    }

    println!();
    let use_config = Confirm::new("Use these saved members and settings?")
        .with_default(true)
        .prompt()?;

    Ok(use_config)
}

#[derive(Debug)]
struct DeploymentResult {
    rpc_url: String,
    multisig_address: Pubkey,
    vault_address: Pubkey,
    transaction_signature: String,
}

#[derive(Tabled)]
struct MemberRow {
    #[tabled(rename = "#")]
    index: usize,
    #[tabled(rename = "Public Key")]
    public_key: String,
    #[tabled(rename = "Permissions")]
    permissions: String,
}

#[derive(Tabled)]
struct DeploymentRow {
    #[tabled(rename = "RPC URL")]
    rpc_url: String,
    #[tabled(rename = "Multisig Address")]
    multisig_address: String,
    #[tabled(rename = "Vault Address (Index 0)")]
    vault_address: String,
}

fn print_deployment_summary(deployments: &[DeploymentResult], members: &[Member], threshold: u16, create_key: &Pubkey) {
    if deployments.is_empty() {
        println!("\n{} No successful deployments to summarize.", "❌".bright_red());
        return;
    }

    println!("\n{}", "🎉 DEPLOYMENT SUMMARY".bright_magenta().bold());
    println!("{}", "═".repeat(80).bright_blue());

    // Configuration section
    println!("\n{}", "📋 Configuration:".bright_yellow().bold());
    println!("  {}: {}", "Create Key".cyan(), create_key.to_string().bright_white());
    println!("  {}: {}", "Threshold".cyan(), threshold.to_string().bright_green());
    println!("  {}: {}", "Total Members".cyan(), members.len().to_string().bright_green());

    // Members table
    println!("\n{}", "👥 Members & Permissions:".bright_yellow().bold());
    let member_rows: Vec<MemberRow> = members
        .iter()
        .enumerate()
        .map(|(i, member)| {
            let perms = decode_permissions(member.permissions.mask);
            let role_indicator = if member.permissions.mask == 1 { " (Contributor)" } else { "" };
            MemberRow {
                index: i + 1,
                public_key: format!("{}{}", member.key.to_string(), role_indicator),
                permissions: perms.join(", "),
            }
        })
        .collect();

    let members_table = Table::new(member_rows)
        .with(Style::rounded())
        .to_string();
    println!("{}", members_table);

    // Deployments table
    println!("\n{}", "🌐 Network Deployments:".bright_yellow().bold());
    let deployment_rows: Vec<DeploymentRow> = deployments
        .iter()
        .map(|deployment| {
            let rpc_display = if deployment.rpc_url.len() > 35 {
                format!("{}...", &deployment.rpc_url[..32])
            } else {
                deployment.rpc_url.clone()
            };

            DeploymentRow {
                rpc_url: rpc_display,
                multisig_address: deployment.multisig_address.to_string(),
                vault_address: deployment.vault_address.to_string(),
            }
        })
        .collect();

    let deployments_table = Table::new(deployment_rows)
        .with(Style::rounded())
        .to_string();
    println!("{}", deployments_table);

    // Transaction signatures
    println!("\n{}", "📜 Transaction Signatures:".bright_yellow().bold());
    for (i, deployment) in deployments.iter().enumerate() {
        let rpc_display = if deployment.rpc_url.len() > 25 {
            format!("{}...", &deployment.rpc_url[..22])
        } else {
            deployment.rpc_url.clone()
        };

        println!("  {}: {} → {}",
                format!("{}.", i + 1).bright_cyan(),
                rpc_display.bright_white(),
                deployment.transaction_signature.bright_green());
    }

    println!("\n{} Successfully deployed to {} network(s)!",
             "✅".bright_green().bold(),
             deployments.len().to_string().bright_green().bold());
    println!("{}", "═".repeat(80).bright_blue());
}
