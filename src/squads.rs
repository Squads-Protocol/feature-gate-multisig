use borsh::{BorshDeserialize, BorshSerialize};
use solana_pubkey::Pubkey;
use std::io::{Read, Write};
use std::marker::PhantomData;

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

pub const SEED_EPHEMERAL_SIGNER: &[u8] = b"ephemeral_signer";

pub const SQUADS_MULTISIG_PROGRAM: Pubkey =
    Pubkey::from_str_const("SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf");

#[derive(BorshDeserialize)]
pub struct ProgramConfig {
    pub authority: Pubkey,
    pub multisig_creation_fee: u64,
    pub treasury: Pubkey,
    pub _reserved: [u8; 64],
}
#[derive(BorshSerialize)]
pub struct MultisigCreateArgsV2 {
    pub config_authority: Option<Pubkey>,
    pub threshold: u16,
    pub members: Vec<Member>,
    pub time_lock: u32,
    pub rent_collector: Option<Pubkey>,
    pub memo: Option<String>,
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

use solana_instruction::AccountMeta;

pub struct MultisigCreateV2Accounts {
    pub create_key: Pubkey,
    pub creator: Pubkey,
    pub multisig: Pubkey,
    pub system_program: Pubkey,
    pub program_config: Pubkey,
    pub treasury: Pubkey,
}

pub struct MultisigCreateTransaction {
    pub multisig: Pubkey,
    pub transaction: Pubkey,
    pub creator: Pubkey,
    pub rent_payer: Pubkey,
    pub system_program: Pubkey,
}

impl MultisigCreateTransaction {
    pub fn to_account_metas(&self, _is_signer: Option<bool>) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.multisig, false),
            AccountMeta::new(self.transaction, false),
            AccountMeta::new_readonly(self.creator, true),
            AccountMeta::new(self.rent_payer, true),
            AccountMeta::new_readonly(self.system_program, false),
        ]
    }
}

pub struct VaultTransactionCreateArgsData {
    pub args: VaultTransactionCreateArgs,
}

#[derive(BorshSerialize, BorshDeserialize)]
pub struct VaultTransactionCreateArgs {
    pub vault_index: u8,
    pub ephemeral_signers: u8,
    pub transaction_message: Vec<u8>,
    pub memo: Option<String>,
}

impl VaultTransactionCreateArgsData {
    pub fn data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(CREATE_TRANSACTION_DISCRIMINATOR);
        data.extend_from_slice(&borsh::to_vec(&self.args).unwrap());
        data
    }
}

impl MultisigCreateV2Accounts {
    pub fn to_account_metas(&self, _is_signer: Option<bool>) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.program_config, false),
            AccountMeta::new(self.treasury, false),
            AccountMeta::new(self.multisig, false),
            AccountMeta::new_readonly(self.create_key, true),
            AccountMeta::new(self.creator, true),
            AccountMeta::new_readonly(self.system_program, false),
        ]
    }
}

pub struct MultisigCreateV2Data {
    pub args: MultisigCreateArgsV2,
}

impl MultisigCreateV2Data {
    pub fn data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(CREATE_MULTISIG_V2_DISCRIMINATOR);
        data.extend_from_slice(&borsh::to_vec(&self.args).unwrap());
        data
    }
}

pub struct MultisigCreateProposalAccounts {
    pub multisig: Pubkey,
    pub proposal: Pubkey,
    pub creator: Pubkey,
    pub rent_payer: Pubkey,
    pub system_program: Pubkey,
}

impl MultisigCreateProposalAccounts {
    pub fn to_account_metas(&self, _is_signer: Option<bool>) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.multisig, false),
            AccountMeta::new(self.proposal, false),
            AccountMeta::new_readonly(self.creator, true),
            AccountMeta::new(self.rent_payer, true),
            AccountMeta::new_readonly(self.system_program, false),
        ]
    }
}

pub struct MultisigCreateProposalData {
    pub args: MultisigCreateProposalArgs,
}

impl MultisigCreateProposalData {
    pub fn data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(CREATE_PROPOSAL_DISCRIMINATOR);
        data.extend_from_slice(&borsh::to_vec(&self.args).unwrap());
        data
    }
}

#[derive(BorshSerialize, BorshDeserialize)]
pub struct MultisigCreateProposalArgs {
    pub transaction_index: u64,
    pub is_draft: bool,
}

// transaction wire structs
#[derive(BorshSerialize, BorshDeserialize, Clone)]
pub struct TransactionMessage {
    /// The number of signer pubkeys in the account_keys vec.
    pub num_signers: u8,
    /// The number of writable signer pubkeys in the account_keys vec.
    pub num_writable_signers: u8,
    /// The number of writable non-signer pubkeys in the account_keys vec.
    pub num_writable_non_signers: u8,
    /// The list of unique account public keys (including program IDs) that will be used in the provided instructions.
    pub account_keys: SmallVec<u8, Pubkey>,
    /// The list of instructions to execute.
    pub instructions: SmallVec<u8, CompiledInstruction>,
    /// List of address table lookups used to load additional accounts
    /// for this transaction.
    pub address_table_lookups: SmallVec<u8, MessageAddressTableLookup>,
}

// Concise serialization schema for instructions that make up transaction.
#[derive(BorshSerialize, BorshDeserialize, Clone)]
pub struct CompiledInstruction {
    pub program_id_index: u8,
    /// Indices into the tx's `account_keys` list indicating which accounts to pass to the instruction.
    pub account_indexes: SmallVec<u8, u8>,
    /// Instruction data.
    pub data: SmallVec<u16, u8>,
}

/// Address table lookups describe an on-chain address lookup table to use
/// for loading more readonly and writable accounts in a single tx.
#[derive(BorshSerialize, BorshDeserialize, Clone)]
pub struct MessageAddressTableLookup {
    /// Address lookup table account key
    pub account_key: Pubkey,
    /// List of indexes used to load writable account addresses
    pub writable_indexes: SmallVec<u8, u8>,
    /// List of indexes used to load readonly account addresses
    pub readonly_indexes: SmallVec<u8, u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SmallVec<L, T>(Vec<T>, PhantomData<L>);

impl<L, T> SmallVec<L, T> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }
}

impl<L, T: PartialEq> SmallVec<L, T> {
    pub fn contains(&self, x: &T) -> bool {
        self.0.contains(x)
    }
}

impl<L, T> std::ops::Index<usize> for SmallVec<L, T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl<L, T> From<SmallVec<L, T>> for Vec<T> {
    fn from(val: SmallVec<L, T>) -> Self {
        val.0
    }
}

impl<L, T> From<Vec<T>> for SmallVec<L, T> {
    fn from(val: Vec<T>) -> Self {
        Self(val, PhantomData)
    }
}

impl<T: BorshSerialize> BorshSerialize for SmallVec<u8, T> {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        let len = u8::try_from(self.len()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        // Write the length of the vector as u8.
        writer.write_all(&len.to_le_bytes())?;

        // Write the vector elements.
        serialize_slice(&self.0, writer)
    }
}

impl<T: BorshSerialize> BorshSerialize for SmallVec<u16, T> {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        let len = u16::try_from(self.len()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        // Write the length of the vector as u16.
        writer.write_all(&len.to_le_bytes())?;

        // Write the vector elements.
        serialize_slice(&self.0, writer)
    }
}

impl<L, T> BorshDeserialize for SmallVec<L, T>
where
    L: BorshDeserialize + Into<u32>,
    T: BorshDeserialize,
{
    /// This implementation almost exactly matches standard implementation of
    /// `Vec<T>::deserialize` except that it uses `L` instead of `u32` for the length,
    /// and doesn't include `unsafe` code.
    fn deserialize_reader<R: Read>(reader: &mut R) -> std::io::Result<Self> {
        let len: u32 = L::deserialize_reader(reader)?.into();

        let vec = if len == 0 {
            Vec::new()
        } else if let Some(vec_bytes) = T::vec_from_reader(len, reader)? {
            vec_bytes
        } else {
            let mut result = Vec::with_capacity(hint::cautious::<T>(len));
            for _ in 0..len {
                result.push(T::deserialize_reader(reader)?);
            }
            result
        };

        Ok(SmallVec(vec, PhantomData))
    }
}

// This is copy-pasted from borsh::de::hint;
mod hint {
    #[inline]
    pub fn cautious<T>(hint: u32) -> usize {
        let el_size = core::mem::size_of::<T>() as u32;
        core::cmp::max(core::cmp::min(hint, 4096 / el_size), 1) as usize
    }
}

/// Helper method that is used to serialize a slice of data (without the length marker).
/// Copied from borsh::ser::serialize_slice.
#[inline]
fn serialize_slice<T: BorshSerialize, W: Write>(data: &[T], writer: &mut W) -> std::io::Result<()> {
    if let Some(u8_slice) = T::u8_slice(data) {
        writer.write_all(u8_slice)?;
    } else {
        for item in data {
            item.serialize(writer)?;
        }
    }
    Ok(())
}
