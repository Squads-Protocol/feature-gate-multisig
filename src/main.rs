mod squads;

use crate::squads::{get_multisig_pda, get_vault_pda};
use anyhow::Result;
use clap::{Parser, Subcommand};
use inquire::{Select, Text};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "feature-gate-multisig-tool")]
#[command(about = "A CLI tool for managing feature gate multisig operations")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Create a new multisig wallet")]
    Create {
        #[arg(short, long, help = "Number of required signatures")]
        threshold: Option<u32>,
        #[arg(short, long, help = "Signers")]
        signers: Option<Vec<String>>,
    },
    #[command(about = "Show feature multisig details")]
    Show {
        #[arg(help = "Feature multisig address")]
        address: Option<String>,
    },
    #[command(about = "Start interactive mode")]
    Interactive,
    #[command(about = "Show current configuration")]
    Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub threshold: u32,
    pub signers: Vec<Vec<String>>,
    pub network: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: 1,
            signers: vec![vec![]],
            network: "https://api.devnet.solana.com".to_string(),
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
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(command) => handle_command(command).await,
        None => interactive_mode().await,
    }
}

async fn handle_command(command: Commands) -> Result<()> {
    let mut config = load_config()?;

    match command {
        Commands::Create { threshold, signers } => {
            let threshold = threshold.unwrap_or_else(|| {
                Text::new(&format!(
                    "Enter threshold (required signatures) [{}]:",
                    config.threshold
                ))
                .prompt()
                .unwrap_or_default()
                .parse()
                .unwrap_or(config.threshold)
            });

            create_feature_multisig(&mut config, threshold, vec![]).await
        }
        Commands::Show { address } => {
            let address = address.unwrap_or_else(|| {
                Text::new("Enter multisig address:")
                    .prompt()
                    .unwrap_or_default()
            });
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
            "Create new multisig wallet" => {
                let threshold: u32 = Text::new(&format!(
                    "Enter threshold (required signatures) [{}]:",
                    config.threshold
                ))
                .prompt()?
                .parse()
                .unwrap_or(config.threshold);

                create_feature_multisig(&mut config, threshold, vec![]).await?;
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
    threshold: u32,
    sub_multisigs: Vec<String>,
) -> Result<()> {
    println!("Creating feature gate multisig");
    let constributor_initiator_keypair = Keypair::new();
    let contributor_pubkey = constributor_initiator_keypair.pubkey();
    let create_key = Keypair::new();

    let address = get_multisig_pda(&create_key.pubkey(), None).0;
    let default_vault = get_vault_pda(&address, 0, None).0;
    save_config(config)?;

    println!("✓ Feature gate multisig created successfully!");
    Ok(())
}

async fn show_multisig(config: &Config, address: &str) -> Result<()> {
    Ok(())
}

async fn show_config(config: &Config) -> Result<()> {
    println!("Configuration:");
    println!("  Config file: {:?}", get_config_path()?);
    println!("  Default threshold: {}", config.threshold);
    println!("  Default signers: {:?}", config.signers);
    Ok(())
}
