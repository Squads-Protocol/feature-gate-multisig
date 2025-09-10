use crate::provision::create_multisig;
use crate::squads::{get_vault_pda, Member, Permissions};
use crate::utils::*;
use anyhow::Result;
use colored::*;
use solana_keypair::Keypair;
use solana_signer::Signer;
use tabled::{settings::Style, Table, Tabled};

pub async fn create_command(
    config: &mut Config,
    threshold: u16,
    _sub_multisigs: Vec<String>,
    keypair_path: Option<String>,
) -> Result<()> {
    println!(
        "{}",
        "🚀 Creating feature gate multisig configuration"
            .bright_cyan()
            .bold()
    );

    // Collect configuration and members
    let (final_threshold, mut members) = review_and_collect_configuration(config, threshold)?;

    // Load fee payer keypair from CLI arg or config
    let fee_payer_keypair = load_fee_payer_keypair(config, keypair_path)?;

    // Create contributor keypair (always separate from fee payer)
    let contributor_keypair = Keypair::new();
    let contributor_pubkey = contributor_keypair.pubkey();

    // Create a persistent create_key for all deployments
    let create_key = Keypair::new();

    // Add contributor as a member with permission 1 (bitmask for Initiate only)
    members.insert(
        0,
        Member {
            key: contributor_pubkey,
            permissions: Permissions { mask: 1 },
        },
    );

    // Display final configuration
    display_final_configuration(
        &contributor_pubkey,
        &create_key.pubkey(),
        &fee_payer_keypair,
        final_threshold,
        &members,
    );

    // Determine network deployment mode and deploy
    let (use_saved_networks, saved_networks) = choose_network_mode(config, true)?;
    
    let deployments = if use_saved_networks && !saved_networks.is_empty() {
        deploy_to_saved_networks(
            &saved_networks,
            &create_key,
            &contributor_keypair,
            &fee_payer_keypair,
            &members,
            final_threshold,
        ).await?
    } else {
        deploy_to_manual_networks(
            config,
            &create_key,
            &contributor_keypair,
            &fee_payer_keypair,
            &members,
            final_threshold,
        ).await?
    };

    // Print summary table
    print_deployment_summary(
        &deployments,
        &members,
        final_threshold,
        &create_key.pubkey(),
    );

    // Save updated configuration (excluding contributor key)
    if !deployments.is_empty() {
        config.threshold = final_threshold;
        config.members = members
            .iter()
            .skip(1) // Skip contributor (index 0)
            .map(|member| member.key.to_string())
            .collect();

        save_config(config)?;
        println!(
            "\n{} Configuration saved for future use",
            "💾".bright_green()
        );
    }

    Ok(())
}

async fn deploy_to_single_network(
    rpc_url: &str,
    network_index: usize,
    total_networks: usize,
    create_key: &Keypair,
    contributor_keypair: &Keypair,
    fee_payer_keypair: &Option<Keypair>,
    members: &[Member],
    threshold: u16,
) -> Result<DeploymentResult> {
    let multisig_address = crate::squads::get_multisig_pda(&create_key.pubkey(), None).0;
    let vault_address = get_vault_pda(&multisig_address, 0, None).0;

    display_deployment_info(
        network_index,
        total_networks,
        rpc_url,
        &create_key.pubkey(),
        &contributor_keypair.pubkey(),
        &multisig_address,
        &vault_address,
        members,
    );

    let signer_for_creation = fee_payer_keypair
        .as_ref()
        .map(|kp| kp as &dyn Signer)
        .unwrap_or(contributor_keypair as &dyn Signer);

    let (multisig_address, signature) = create_multisig(
        rpc_url.to_string(),
        None, // Use default program ID
        signer_for_creation,
        create_key,
        None, // No config authority
        members.to_vec(),
        threshold,
        None,       // No rent collector
        Some(5000), // Priority fee
    ).await.map_err(|e| anyhow::anyhow!("Failed to create multisig: {}", e))?;

    let vault_address = get_vault_pda(&multisig_address, 0, None).0;

    println!(
        "{} Deployment successful on {}",
        "✅".bright_green(),
        rpc_url.bright_white()
    );

    // Create both activation and revocation transactions
    create_and_send_transaction_proposal(
        rpc_url,
        fee_payer_keypair,
        contributor_keypair,
        &multisig_address,
        "activation",
        1, // Transaction index 1 (activation)
    ).await?;

    create_and_send_transaction_proposal(
        rpc_url,
        fee_payer_keypair,
        contributor_keypair,
        &multisig_address,
        "revocation",
        2, // Transaction index 2 (revocation)
    ).await?;

    Ok(DeploymentResult {
        rpc_url: rpc_url.to_string(),
        multisig_address,
        vault_address,
        transaction_signature: signature,
    })
}

async fn deploy_to_saved_networks(
    networks: &[String],
    create_key: &Keypair,
    contributor_keypair: &Keypair,
    fee_payer_keypair: &Option<Keypair>,
    members: &[Member],
    threshold: u16,
) -> Result<Vec<DeploymentResult>> {
    println!("\n{} Deploying to all saved networks", "🚀".bright_cyan());
    
    let mut deployments = Vec::new();

    for (i, rpc_url) in networks.iter().enumerate() {
        match deploy_to_single_network(
            rpc_url,
            i,
            networks.len(),
            create_key,
            contributor_keypair,
            fee_payer_keypair,
            members,
            threshold,
        ).await {
            Ok(deployment) => {
                deployments.push(deployment);
            }
            Err(e) => {
                println!(
                    "{} Failed to deploy on {}: {}",
                    "❌".bright_red(),
                    rpc_url.bright_white(),
                    e.to_string().red()
                );
            }
        }

        if i < networks.len() - 1 {
            println!("\n{} Proceeding to next network...", "⏳".bright_yellow());
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    Ok(deployments)
}

async fn deploy_to_manual_networks(
    config: &Config,
    create_key: &Keypair,
    contributor_keypair: &Keypair,
    fee_payer_keypair: &Option<Keypair>,
    members: &[Member],
    threshold: u16,
) -> Result<Vec<DeploymentResult>> {
    println!("\n{} Manual network entry mode", "🔄".bright_cyan());
    
    let mut deployments = Vec::new();

    loop {
        let rpc_url = prompt_for_network(config)?;

        match deploy_to_single_network(
            &rpc_url,
            0,
            1,
            create_key,
            contributor_keypair,
            fee_payer_keypair,
            members,
            threshold,
        ).await {
            Ok(deployment) => {
                deployments.push(deployment);
            }
            Err(e) => {
                println!(
                    "{} Failed to deploy on {}: {}",
                    "❌".bright_red(),
                    rpc_url.bright_white(),
                    e.to_string().red()
                );
            }
        }

        let deploy_another =
            inquire::Confirm::new("Deploy to another network with the same configuration?")
                .with_default(false)
                .prompt()?;

        if !deploy_another {
            break;
        }

        println!();
    }

    Ok(deployments)
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

fn print_deployment_summary(
    deployments: &[DeploymentResult],
    members: &[Member],
    threshold: u16,
    create_key: &solana_pubkey::Pubkey,
) {
    if deployments.is_empty() {
        println!(
            "\n{} No successful deployments to summarize.",
            "❌".bright_red()
        );
        return;
    }

    println!("\n{}", "🎉 DEPLOYMENT SUMMARY".bright_magenta().bold());
    println!("{}", "═".repeat(80).bright_blue());

    // Configuration section
    println!("\n{}", "📋 Configuration:".bright_yellow().bold());
    println!(
        "  {}: {}",
        "Create Key".cyan(),
        create_key.to_string().bright_white()
    );
    println!(
        "  {}: {}",
        "Threshold".cyan(),
        threshold.to_string().bright_green()
    );
    println!(
        "  {}: {}",
        "Total Members".cyan(),
        members.len().to_string().bright_green()
    );

    // Members table
    println!("\n{}", "👥 Members & Permissions:".bright_yellow().bold());
    let member_rows: Vec<MemberRow> = members
        .iter()
        .enumerate()
        .map(|(i, member)| {
            let perms = decode_permissions(member.permissions.mask);
            let role_indicator = if member.permissions.mask == 1 {
                " (Contributor)"
            } else {
                ""
            };
            MemberRow {
                index: i + 1,
                public_key: format!("{}{}", member.key.to_string(), role_indicator),
                permissions: perms.join(", "),
            }
        })
        .collect();

    let members_table = Table::new(member_rows).with(Style::rounded()).to_string();
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

        println!(
            "  {}: {} → {}",
            format!("{}.", i + 1).bright_cyan(),
            rpc_display.bright_white(),
            deployment.transaction_signature.bright_green()
        );
    }

    println!(
        "\n{} Successfully deployed to {} network(s)!",
        "✅".bright_green().bold(),
        deployments.len().to_string().bright_green().bold()
    );
    println!("{}", "═".repeat(80).bright_blue());
}