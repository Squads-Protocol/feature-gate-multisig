use crate::utils::*;
use anyhow::Result;
use colored::*;

pub async fn config_command(config: &Config) -> Result<()> {
    println!("{}", "📋 Configuration:".bright_yellow().bold());
    println!(
        "  {}: {:?}",
        "Config file".cyan(),
        get_config_path()?.to_string_lossy().bright_white()
    );
    println!(
        "  {}: {}",
        "Default threshold".cyan(),
        config.threshold.to_string().bright_green()
    );

    // Display fee payer path
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

    // Display networks array if available, otherwise show legacy single network
    if !config.networks.is_empty() {
        println!(
            "  {}: {} networks",
            "Saved networks".cyan(),
            config.networks.len().to_string().bright_green()
        );
        for (i, network) in config.networks.iter().enumerate() {
            println!(
                "    {}: {}",
                format!("Network {}", i + 1).cyan(),
                network.bright_white()
            );
        }
    } else if !config.network.is_empty() {
        println!(
            "  {}: {}",
            "Default network".cyan(),
            config.network.bright_white()
        );
    }

    println!(
        "  {}: {} members",
        "Saved members".cyan(),
        config.members.len().to_string().bright_green()
    );

    if !config.members.is_empty() {
        for (i, member) in config.members.iter().enumerate() {
            println!(
                "    {}: {}",
                format!("Member {}", i + 1).cyan(),
                member.bright_white()
            );
        }
    }

    Ok(())
}