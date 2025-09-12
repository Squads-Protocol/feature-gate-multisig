use crate::commands::{
    approve_feature_gate_activation_proposal, config_command, create_command, show_command,
};
use crate::squads::get_vault_pda;
use crate::utils::*;
use eyre::Result;
use inquire::{Confirm, Select, Text};
use solana_pubkey::Pubkey;

pub async fn interactive_mode() -> Result<()> {
    let mut config = load_config()?;

    loop {
        let options = vec![
            "Create new feature gate multisig",
            "Show feature gate multisig details",
            "Show configuration",
            "Transaction Generation",
            "Exit",
        ];

        let choice: &str = Select::new("What would you like to do?", options).prompt()?;

        match choice {
            "Create new feature gate multisig" => {
                let feepayer_path = prompt_for_fee_payer_path(&config)?;
                create_command(&mut config, None, vec![], Some(feepayer_path)).await?;
            }
            "Show feature gate multisig details" => {
                let address = Text::new("Enter the main multisig address:").prompt()?;
                show_command(&config, Some(address)).await?;
            }
            "Show configuration" => {
                config_command(&config).await?;
            }
            "Transaction Generation" => {
                let feature_gate_multisig_address =
                    prompt_for_pubkey("Enter the feature gate multisig address:")?;
                let feature_gate_id = get_vault_pda(&feature_gate_multisig_address, 0, None).0;
                let fee_payer_path = prompt_for_fee_payer_path(&config)?;

                let options = vec!["Approve feature gate activation proposal", "Exit"];
                let choice: &str = Select::new("What would you like to do?", options).prompt()?;
                match choice {
                    "Approve feature gate activation proposal" => {
                        Confirm::new(&format!(
                            "You're approving the activation of the following feature gate: {}?",
                            feature_gate_id
                        ))
                        .with_default(true)
                        .prompt()?;
                        let voting_key = prompt_for_pubkey(
                            "Enter the voting key: (Can be either EOA or parent multisig)",
                        )?;
                        approve_feature_gate_activation_proposal(
                            &config,
                            feature_gate_multisig_address,
                            voting_key,
                            fee_payer_path,
                            None,
                        )
                        .await?;
                    }
                    "Exit" => break,
                    _ => unreachable!(),
                }
            }
            "Exit" => break,
            _ => unreachable!(),
        }

        println!("\n");
    }

    Ok(())
}
