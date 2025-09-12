use crate::output::Output;
use crate::utils::*;
use eyre::Result;

pub async fn config_command(config: &Config) -> Result<()> {
    Output::header("📋 Configuration:");
    Output::field("Config file", &get_config_path()?.to_string_lossy());
    Output::field("Default threshold", &config.threshold.to_string());

    // Display fee payer path
    Output::config_item("Fee payer keypair", &config.fee_payer_path.as_deref().unwrap_or(""));

    // Display networks
    if !config.networks.is_empty() {
        Output::field("Saved networks", &format!("{} networks", config.networks.len()));
        for (i, network) in config.networks.iter().enumerate() {
            Output::numbered_field(i + 1, "Network", network);
        }
    } else {
        Output::config_item("Networks", "None configured");
    }

    Output::field("Saved members", &format!("{} members", config.members.len()));

    if !config.members.is_empty() {
        for (i, member) in config.members.iter().enumerate() {
            Output::numbered_field(i + 1, "Member", member);
        }
    }

    Ok(())
}