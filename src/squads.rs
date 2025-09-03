use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

pub const CREATE_MULTISIG_V2_DISCRIMINATOR: &[u8] = &[50, 221, 199, 93, 40, 245, 139, 233];

pub const CREATE_TRANSACTION_DISCRIMINATOR: &[u8] = &[48, 250, 78, 168, 208, 226, 218, 211];

pub const CREATE_PROPOSAL_DISCRIMINATOR: &[u8] = &[220, 60, 73, 224, 30, 108, 79, 159];

pub const PROPOSAL_APPROVE_DISCRIMINATOR: &[u8] = &[144, 37, 164, 136, 188, 216, 42, 248];

pub const PROPOSAL_REJECT_DISCRIMINATOR: &[u8] = &[243, 62, 134, 156, 230, 106, 246, 135];

pub const EXECUTE_TRANSACTION_DISCRIMINATOR: &[u8] = &[194, 8, 161, 87, 153, 164, 25, 171];

pub const SQUADS_MULTISIG_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf");

pub const SEED_PREFIX: &[u8] = b"multisig";
pub const SEED_PROGRAM_CONFIG: &[u8] = b"program_config";
pub const SEED_MULTISIG: &[u8] = b"multisig";
pub const SEED_PROPOSAL: &[u8] = b"proposal";
pub const SEED_TRANSACTION: &[u8] = b"transaction";
pub const SEED_VAULT: &[u8] = b"vault";

#[derive(BorshDeserialize, BorshSerialize, Eq, PartialEq, Clone)]
pub struct Multisig {
    pub create_key: Pubkey,
    pub config_authority: Pubkey,
    pub threshold: u16,
    pub time_lock: u32,
    pub transaction_index: u64,
    pub stale_transaction_index: u64,
    pub rent_collector: Option<Pubkey>,
    pub bump: u8,
    pub members: Vec<Member>,
}

#[derive(BorshDeserialize, BorshSerialize, Eq, PartialEq, Clone)]
pub struct Member {
    pub key: Pubkey,
    pub permissions: Permissions,
}

#[derive(Clone, Copy)]
pub enum Permission {
    Initiate = 1 << 0,
    Vote = 1 << 1,
    Execute = 1 << 2,
}

#[derive(BorshSerialize, BorshDeserialize, Eq, PartialEq, Clone, Copy, Default, Debug)]
pub struct Permissions {
    pub mask: u8,
}

#[derive(BorshSerialize, BorshDeserialize)]
pub struct VaultTransaction {
    pub multisig: Pubkey,
    pub creator: Pubkey,
    pub index: u64,
    pub bump: u8,
    pub vault_index: u8,
    pub vault_bump: u8,
    pub ephemeral_signer_bumps: Vec<u8>,
    pub message: VaultTransactionMessage,
}

#[derive(BorshDeserialize, BorshSerialize, Clone)]
pub struct VaultTransactionMessage {
    pub num_signers: u8,
    pub num_writable_signers: u8,
    pub num_writable_non_signers: u8,
    pub account_keys: Vec<Pubkey>,
    pub instructions: Vec<MultisigCompiledInstruction>,
    pub address_table_lookups: Vec<MultisigMessageAddressTableLookup>,
}

#[derive(BorshDeserialize, BorshSerialize, Clone)]
pub struct MultisigCompiledInstruction {
    pub program_id_index: u8,
    pub account_indexes: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(BorshDeserialize, BorshSerialize, Clone)]
pub struct MultisigMessageAddressTableLookup {
    pub account_key: Pubkey,
    pub writable_indexes: Vec<u8>,
    pub readonly_indexes: Vec<u8>,
}

pub fn get_program_config_pda(program_id: Option<&Pubkey>) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_PREFIX, SEED_PROGRAM_CONFIG],
        program_id.unwrap_or(&SQUADS_MULTISIG_PROGRAM_ID),
    )
}

pub fn get_multisig_pda(create_key: &Pubkey, program_id: Option<&Pubkey>) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_PREFIX, SEED_MULTISIG, create_key.to_bytes().as_ref()],
        program_id.unwrap_or(&SQUADS_MULTISIG_PROGRAM_ID),
    )
}

pub fn get_vault_pda(
    multisig_pda: &Pubkey,
    index: u8,
    program_id: Option<&Pubkey>,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            SEED_PREFIX,
            multisig_pda.to_bytes().as_ref(),
            SEED_VAULT,
            &[index],
        ],
        program_id.unwrap_or(&SQUADS_MULTISIG_PROGRAM_ID),
    )
}

pub fn get_transaction_pda(
    multisig_pda: &Pubkey,
    transaction_index: u64,
    program_id: Option<&Pubkey>,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            SEED_PREFIX,
            multisig_pda.to_bytes().as_ref(),
            SEED_TRANSACTION,
            transaction_index.to_le_bytes().as_ref(),
        ],
        program_id.unwrap_or(&SQUADS_MULTISIG_PROGRAM_ID),
    )
}

pub fn get_proposal_pda(
    multisig_pda: &Pubkey,
    transaction_index: u64,
    program_id: Option<&Pubkey>,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            SEED_PREFIX,
            multisig_pda.to_bytes().as_ref(),
            SEED_TRANSACTION,
            &transaction_index.to_le_bytes(),
            SEED_PROPOSAL,
        ],
        program_id.unwrap_or(&SQUADS_MULTISIG_PROGRAM_ID),
    )
}
