use crate::commands::show::print_members_table;
use crate::constants::DEFAULT_DEVNET_URL;
use crate::output::Output;
use crate::provision::{create_rpc_client, fetch_squads_multisig};
use crate::squads::{Multisig, PERMISSION_VOTE};
use crate::utils::{get_network_display, is_mainnet, validate_pubkey_with_retry, Config};
use crate::verification::{
    program_warnings, verify_feature_gate, verify_squads_program, FeatureVerification,
    ProgramAuthenticity,
};
use eyre::Result;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use std::str::FromStr;

/// Verify a feature gate multisig: the Squads program is authentic, the feature
/// account is in the expected state, and who the owners are. Read-only, and
/// runs across every configured network.
pub fn verify_command(config: &Config, address: Option<String>) -> Result<()> {
    let multisig = match address {
        Some(addr) => {
            Pubkey::from_str(&addr).map_err(|_| eyre::eyre!("Invalid multisig address format"))?
        }
        None => validate_pubkey_with_retry("Enter the feature gate multisig address:")?,
    };

    let networks = if config.networks.is_empty() {
        vec![DEFAULT_DEVNET_URL.to_string()]
    } else {
        config.networks.clone()
    };

    Output::header(&format!("🔎 Verifying feature gate multisig {multisig}"));

    for network in &networks {
        println!();
        Output::header(&format!(
            "🌐 {} ({})",
            get_network_display(network),
            network
        ));
        verify_on_network(&create_rpc_client(network), &multisig, is_mainnet(network));
    }

    Ok(())
}

fn verify_on_network(rpc: &RpcClient, multisig: &Pubkey, is_mainnet: bool) {
    match verify_squads_program(rpc) {
        Ok(p) => display_program_authenticity(&p, is_mainnet),
        Err(e) => Output::warning(&format!("Could not verify Squads program: {e}")),
    }
    match verify_feature_gate(rpc, multisig) {
        Ok(v) => display_feature(&v),
        Err(e) => Output::warning(&format!("Could not read feature gate account: {e}")),
    }
    match fetch_squads_multisig(rpc, multisig, "multisig") {
        Ok(ms) => display_owners(&ms),
        Err(e) => Output::warning(&format!("Multisig not readable on this network: {e}")),
    }
}

fn display_program_authenticity(p: &ProgramAuthenticity, is_mainnet: bool) {
    println!();
    Output::header("⚙️ Squads Program Authenticity");
    Output::field("Program", &p.program_id.to_string());
    Output::field("Executable", &p.executable.to_string());
    Output::field("Owned by upgradeable loader", &p.loader_owner_ok.to_string());

    if is_mainnet {
        Output::field("Immutable (frozen)", &p.immutable.to_string());
        Output::field("Bytecode hash matches", &p.hash_matches.to_string());
    } else {
        Output::field(
            "Upgrade authority",
            &p.upgrade_authority
                .map(|a| a.to_string())
                .unwrap_or_else(|| "none".to_string()),
        );
        Output::info(
            "Immutability and the verified bytecode hash are only asserted on mainnet; Squads keeps its devnet/testnet deployments upgradeable.",
        );
    }

    let warnings = program_warnings(p, is_mainnet);
    if warnings.is_empty() {
        if is_mainnet {
            Output::success("Program is the authentic, immutable Squads v4.");
        } else {
            Output::success("Squads program is present and loaded by the upgradeable loader.");
        }
    } else {
        for warning in warnings {
            Output::warning(&warning);
        }
    }
}

fn display_feature(v: &FeatureVerification) {
    println!();
    Output::header("🪄 Feature Gate");
    Output::field("Feature gate ID (vault 0)", &v.feature_id.to_string());
    Output::field("Status", &format!("{:?}", v.status));
    Output::field("Lamports", &v.lamports.to_string());
    Output::field("Rent-exempt", &v.rent_exempt.to_string());
}

fn display_owners(ms: &Multisig) {
    let voting_members = ms
        .members
        .iter()
        .filter(|m| m.permissions.mask & PERMISSION_VOTE != 0)
        .count();

    println!();
    Output::header("👥 Owners");
    Output::field(
        "Threshold",
        &format!("{} of {} voting members", ms.threshold, voting_members),
    );
    print_members_table(ms);
}
