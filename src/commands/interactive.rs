use crate::commands::{config_command, create_command, show_command};
use crate::utils::*;
use eyre::Result;
use inquire::{Select, Text};

pub async fn interactive_mode() -> Result<()> {
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
                let threshold = prompt_for_threshold(&config)?;
                let feepayer_path = prompt_for_fee_payer_path(&config)?;

                create_command(&mut config, threshold, vec![], Some(feepayer_path)).await?;
            }
            "Show feature gate multisig details" => {
                let address = Text::new("Enter the main multisig address:").prompt()?;
                show_command(&config, Some(address)).await?;
            }
            "Show configuration" => {
                config_command(&config).await?;
            }
            "Exit" => break,
            _ => unreachable!(),
        }

        println!("\n");
    }

    Ok(())
}