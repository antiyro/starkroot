use blockifier::state::cached_state::CommitmentStateDiff;
use indexmap::IndexMap;
use mp_convert::field_element::FromFieldElement;
use mp_felt::Felt252Wrapper;
use mp_hashers::poseidon::PoseidonHasher;
use mp_hashers::HasherT;
use starknet_api::core::{ClassHash, CompiledClassHash, ContractAddress, Nonce};
use starknet_api::hash::StarkFelt;
use starknet_api::state::StorageKey;
use starknet_api::transaction::{Event, Transaction};
use starknet_core::types::{
    ContractStorageDiffItem, DeclaredClassItem, DeployedContractItem, NonceUpdate, ReplacedClassItem, StateUpdate,
    StorageEntry,
};
use starknet_ff::FieldElement;

use super::classes::class_trie_root;
use super::contracts::contract_trie_root;
use super::events::memory_event_commitment;
use super::transactions::memory_transaction_commitment;

/// Calculate the transaction and event commitment.
///
/// # Arguments
///
/// * `transactions` - The transactions of the block
/// * `events` - The events of the block
/// * `chain_id` - The current chain id
/// * `block_number` - The current block number
///
/// # Returns
///
/// The transaction and the event commitment as `Felt252Wrapper`.
pub fn calculate_tx_and_event_commitments(
    transactions: &[Transaction],
    events: &[Event],
    chain_id: Felt252Wrapper,
    block_number: u64,
) -> (Felt252Wrapper, Felt252Wrapper) {
    let (commitment_tx, commitment_event) = rayon::join(
        || memory_transaction_commitment(transactions, chain_id, block_number),
        || memory_event_commitment(events),
    );
    (
        commitment_tx.expect("Failed to calculate transaction commitment"),
        commitment_event.expect("Failed to calculate event commitment"),
    )
}

/// Aggregates all the changes from last state update in a way that is easy to access
/// when computing the state root
///
/// * `state_update`: The last state update fetched from the sequencer
pub fn build_commitment_state_diff(state_update: &StateUpdate) -> CommitmentStateDiff {
    let mut commitment_state_diff = CommitmentStateDiff {
        address_to_class_hash: IndexMap::new(),
        address_to_nonce: IndexMap::new(),
        storage_updates: IndexMap::new(),
        class_hash_to_compiled_class_hash: IndexMap::new(),
    };

    for DeployedContractItem { address, class_hash } in state_update.state_diff.deployed_contracts.iter() {
        let address = ContractAddress::from_field_element(address);
        let class_hash = if address == ContractAddress::from_field_element(FieldElement::ZERO) {
            // System contracts doesnt have class hashes
            ClassHash::from_field_element(FieldElement::ZERO)
        } else {
            ClassHash::from_field_element(class_hash)
        };
        commitment_state_diff.address_to_class_hash.insert(address, class_hash);
    }

    for ReplacedClassItem { contract_address, class_hash } in state_update.state_diff.replaced_classes.iter() {
        let address = ContractAddress::from_field_element(contract_address);
        let class_hash = ClassHash::from_field_element(class_hash);
        commitment_state_diff.address_to_class_hash.insert(address, class_hash);
    }

    for DeclaredClassItem { class_hash, compiled_class_hash } in state_update.state_diff.declared_classes.iter() {
        let class_hash = ClassHash::from_field_element(class_hash);
        let compiled_class_hash = CompiledClassHash::from_field_element(compiled_class_hash);
        commitment_state_diff.class_hash_to_compiled_class_hash.insert(class_hash, compiled_class_hash);
    }

    for NonceUpdate { contract_address, nonce } in state_update.state_diff.nonces.iter() {
        let contract_address = ContractAddress::from_field_element(contract_address);
        let nonce_value = Nonce::from_field_element(nonce);
        commitment_state_diff.address_to_nonce.insert(contract_address, nonce_value);
    }

    for ContractStorageDiffItem { address, storage_entries } in state_update.state_diff.storage_diffs.iter() {
        let contract_address = ContractAddress::from_field_element(address);
        let mut storage_map = IndexMap::new();
        for StorageEntry { key, value } in storage_entries.iter() {
            let key = StorageKey::from_field_element(key);
            let value = StarkFelt::from_field_element(value);
            storage_map.insert(key, value);
        }
        commitment_state_diff.storage_updates.insert(contract_address, storage_map);
    }

    commitment_state_diff
}

/// Calculate state commitment hash value.
///
/// The state commitment is the digest that uniquely (up to hash collisions) encodes the state.
/// It combines the roots of two binary Merkle-Patricia tries of height 251 using Poseidon/Pedersen
/// hashers.
///
/// # Arguments
///
/// * `contracts_trie_root` - The root of the contracts trie.
/// * `classes_trie_root` - The root of the classes trie.
///
/// # Returns
///
/// The state commitment as a `Felt252Wrapper`.
pub fn calculate_state_root<H: HasherT>(
    contracts_trie_root: Felt252Wrapper,
    classes_trie_root: Felt252Wrapper,
) -> Felt252Wrapper
where
    H: HasherT,
{
    let starknet_state_prefix = Felt252Wrapper::try_from("STARKNET_STATE_V0".as_bytes()).unwrap();

    if classes_trie_root == Felt252Wrapper::ZERO {
        contracts_trie_root
    } else {
        let state_commitment_hash =
            H::compute_hash_on_elements(&[starknet_state_prefix.0, contracts_trie_root.0, classes_trie_root.0]);

        state_commitment_hash.into()
    }
}

/// Update the state commitment hash value.
///
/// The state commitment is the digest that uniquely (up to hash collisions) encodes the state.
/// It combines the roots of two binary Merkle-Patricia tries of height 251 using Poseidon/Pedersen
/// hashers.
///
/// # Arguments
///
/// * `CommitmentStateDiff` - The commitment state diff inducing unprocessed state changes.
/// * `BonsaiDb` - The database responsible for storing computing the state tries.
///
///
/// The updated state root as a `Felt252Wrapper`.
pub fn update_state_root(csd: CommitmentStateDiff, block_number: u64) -> Felt252Wrapper {
    // Update contract and its storage tries
    let (contract_trie_root, class_trie_root) = rayon::join(
        || contract_trie_root(&csd, block_number).expect("Failed to compute contract root"),
        || class_trie_root(&csd, block_number).expect("Failed to compute class root"),
    );
    calculate_state_root::<PoseidonHasher>(contract_trie_root, class_trie_root)
}
