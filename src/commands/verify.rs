use crate::commands::show::print_members_table;
use crate::constants::DEFAULT_DEVNET_URL;
use crate::output::Output;
use crate::provision::{create_rpc_client, fetch_squads_multisig};
use crate::squads::{Multisig, PERMISSION_VOTE};
use crate::utils::{get_network_display, is_mainnet, validate_pubkey_with_retry, Config};
use crate::verification::{
    config_fingerprint, is_autonomous, is_mainnet_cluster, is_rekeyed, member_set_warnings,
    multisig_safety_warnings, program_warnings, verify_feature_gate, verify_squads_program,
    FeatureVerification, ProgramAuthenticity,
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

    let mut seen = Vec::new();
    // Warn-and-exit-0 reads as "verified" to anything checking the exit code, so
    // a check that could not run - or ran and reported a problem - has to fail.
    let mut incomplete = false;
    for network in &networks {
        println!();
        Output::header(&format!(
            "🌐 {} ({})",
            get_network_display(network),
            network
        ));
        let rpc = create_rpc_client(network);
        // Identify the cluster from its genesis hash rather than trusting the URL;
        // fall back to the URL heuristic only if the RPC call fails.
        let mainnet = match is_mainnet_cluster(&rpc) {
            Ok(mainnet) => mainnet,
            Err(e) => {
                let fallback = is_mainnet(network);
                Output::warning(&format!(
                    "Could not detect cluster from genesis hash ({e}); \
                     falling back to URL heuristic (mainnet: {fallback})."
                ));
                incomplete = true;
                fallback
            }
        };
        let (found, failed) = verify_on_network(&rpc, &multisig, mainnet);
        incomplete |= failed;
        if let Some(ms) = found {
            seen.push((get_network_display(network), ms));
        }
    }

    // Drift between clusters means at least one deployment is tampered or stale.
    incomplete |= report_cross_network_consistency(&seen);

    if incomplete {
        return Err(eyre::eyre!(
            "Verification failed: see the warnings above. The multisig has not been shown to be correct."
        ));
    }
    Ok(())
}

/// Returns the multisig if readable, and whether any check failed - either
/// because it could not run, or because it ran and reported a problem. The
/// latter is the stronger negative of the two, so it fails the command too.
fn verify_on_network(
    rpc: &RpcClient,
    multisig: &Pubkey,
    is_mainnet: bool,
) -> (Option<Multisig>, bool) {
    let mut failed = false;
    match verify_squads_program(rpc) {
        Ok(p) => failed |= display_program_authenticity(&p, is_mainnet),
        Err(e) => {
            Output::warning(&format!("Could not verify Squads program: {e}"));
            failed = true;
        }
    }
    match verify_feature_gate(rpc, multisig) {
        Ok(v) => failed |= display_feature(&v),
        Err(e) => {
            Output::warning(&format!("Could not read feature gate account: {e}"));
            failed = true;
        }
    }
    match fetch_squads_multisig(rpc, multisig, "multisig") {
        Ok(ms) => {
            failed |= display_owners(&ms);
            (Some(ms), failed)
        }
        Err(e) => {
            Output::warning(&format!("Multisig not readable on this network: {e}"));
            (None, true)
        }
    }
}

/// Flag governance-config drift across networks. The tool deploys identical
/// config everywhere, so any difference between the clusters where the multisig
/// exists points to a tampered or stale deployment.
/// Returns true when the deployments disagree.
pub(crate) fn report_cross_network_consistency(seen: &[(&str, Multisig)]) -> bool {
    if seen.len() < 2 {
        return false;
    }
    println!();
    Output::header("🔗 Cross-network Consistency");

    let (first_net, first_ms) = &seen[0];
    let reference = config_fingerprint(first_ms);
    let mismatched: Vec<&str> = seen
        .iter()
        .skip(1)
        .filter(|(_, ms)| config_fingerprint(ms) != reference)
        .map(|(net, _)| *net)
        .collect();

    if mismatched.is_empty() {
        // Name the networks compared: "all N" counted only the endpoints that
        // answered, so a dropped cluster shrank the claim without saying so.
        Output::success(&format!(
            "Multisig config is identical across the {} networks compared ({}).",
            seen.len(),
            seen.iter()
                .map(|(net, _)| *net)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        false
    } else {
        Output::warning(&format!(
            "Multisig config on {} differs from {}; the deployment is inconsistent across networks.",
            mismatched.join(", "),
            first_net
        ));
        true
    }
}

/// Returns true when the program failed authenticity checks.
fn display_program_authenticity(p: &ProgramAuthenticity, is_mainnet: bool) -> bool {
    println!();
    Output::header("⚙️ Squads Program Authenticity");
    Output::field("Program", &p.program_id.to_string());
    Output::field("Executable", &p.executable.to_string());
    Output::field(
        "Owned by upgradeable loader",
        &p.loader_owner_ok.to_string(),
    );

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
        false
    } else {
        for warning in &warnings {
            Output::warning(warning);
        }
        true
    }
}

/// Returns true when the feature account is in an unexpected state - an owner or
/// data length this tool does not recognise.
fn display_feature(v: &FeatureVerification) -> bool {
    println!();
    Output::header("🪄 Feature Gate");
    Output::field("Feature gate ID (vault 0)", &v.feature_id.to_string());
    Output::field("Status", &format!("{:?}", v.status));
    Output::field("Lamports", &v.lamports.to_string());
    Output::field("Rent-exempt", &v.rent_exempt.to_string());
    if let crate::verification::FeatureGateStatus::Unexpected { owner, data_len } = &v.status {
        Output::warning(&format!(
            "Feature gate account is in an unexpected state: owner {owner}, {data_len} bytes."
        ));
        return true;
    }
    false
}

/// Returns true when the owner set is not binding, i.e. a config authority can
/// rewrite members and threshold unilaterally. A time lock or a completed rekey
/// are intended states and only warn.
fn display_owners(ms: &Multisig) -> bool {
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
    Output::field(
        "Autonomous (config by vote)",
        &is_autonomous(ms).to_string(),
    );
    Output::field("Time lock", &format!("{}s", ms.time_lock));
    if is_rekeyed(ms) {
        Output::warning(
            "This multisig has been rekeyed: its voting keys cannot meet the threshold, so no proposal can ever pass and the configuration is permanently frozen.",
        );
    }
    print_members_table(ms);

    for warning in multisig_safety_warnings(ms) {
        Output::warning(&warning);
    }
    for warning in member_set_warnings(ms) {
        Output::warning(&warning);
    }

    !is_autonomous(ms)
}
