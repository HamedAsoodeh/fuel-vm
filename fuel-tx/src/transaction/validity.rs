use crate::{
    output,
    ConsensusParameters,
    Input,
    Output,
    Witness,
};
use core::hash::Hash;

use fuel_types::{
    canonical,
    AssetId,
    BlockHeight,
};

use crate::Transaction;

use fuel_types::{
    Address,
    Bytes32,
    ChainId,
};

use hashbrown::HashMap;
use itertools::Itertools;

mod error;

#[cfg(test)]
mod tests;

use crate::{
    input::{
        coin::{
            CoinPredicate,
            CoinSigned,
        },
        message::{
            MessageCoinPredicate,
            MessageCoinSigned,
            MessageDataPredicate,
            MessageDataSigned,
        },
    },
    transaction::{
        consensus_parameters::{
            PredicateParameters,
            TxParameters,
        },
        field,
        Executable,
    },
};
pub use error::CheckError;

impl Input {
    pub fn check(
        &self,
        index: usize,
        txhash: &Bytes32,
        outputs: &[Output],
        witnesses: &[Witness],
        predicate_params: &PredicateParameters,
        recovery_cache: &mut Option<HashMap<u8, Address>>,
    ) -> Result<(), CheckError> {
        self.check_without_signature(index, outputs, witnesses, predicate_params)?;
        self.check_signature(index, txhash, witnesses, recovery_cache)?;

        Ok(())
    }

    pub fn check_signature(
        &self,
        index: usize,
        txhash: &Bytes32,
        witnesses: &[Witness],
        recovery_cache: &mut Option<HashMap<u8, Address>>,
    ) -> Result<(), CheckError> {
        match self {
            Self::CoinSigned(CoinSigned {
                witness_index,
                owner,
                ..
            })
            | Self::MessageCoinSigned(MessageCoinSigned {
                witness_index,
                recipient: owner,
                ..
            })
            | Self::MessageDataSigned(MessageDataSigned {
                witness_index,
                recipient: owner,
                ..
            }) => {
                // Helper function for recovering the address from a witness
                let recover_address = || -> Result<Address, CheckError> {
                    let witness = witnesses
                        .get(*witness_index as usize)
                        .ok_or(CheckError::InputWitnessIndexBounds { index })?;

                    witness.recover_witness(txhash, *witness_index as usize)
                };

                // recover the address associated with a witness, using the cache if
                // available
                let recovered_address = if let Some(cache) = recovery_cache {
                    if let Some(recovered_address) = cache.get(witness_index) {
                        *recovered_address
                    } else {
                        // if this witness hasn't been recovered before,
                        // cache ecrecover by witness index
                        let recovered_address = recover_address()?;
                        cache.insert(*witness_index, recovered_address);
                        recovered_address
                    }
                } else {
                    recover_address()?
                };

                if owner != &recovered_address {
                    return Err(CheckError::InputInvalidSignature { index })
                }

                Ok(())
            }

            Self::CoinPredicate(CoinPredicate {
                owner, predicate, ..
            })
            | Self::MessageCoinPredicate(MessageCoinPredicate {
                recipient: owner,
                predicate,
                ..
            })
            | Self::MessageDataPredicate(MessageDataPredicate {
                recipient: owner,
                predicate,
                ..
            }) if !Input::is_predicate_owner_valid(owner, predicate) => {
                Err(CheckError::InputPredicateOwner { index })
            }

            _ => Ok(()),
        }
    }

    pub fn check_without_signature(
        &self,
        index: usize,
        outputs: &[Output],
        witnesses: &[Witness],
        predicate_params: &PredicateParameters,
    ) -> Result<(), CheckError> {
        match self {
            Self::CoinPredicate(CoinPredicate { predicate, .. })
            | Self::MessageCoinPredicate(MessageCoinPredicate { predicate, .. })
            | Self::MessageDataPredicate(MessageDataPredicate { predicate, .. })
                if predicate.is_empty() =>
            {
                Err(CheckError::InputPredicateEmpty { index })
            }

            Self::CoinPredicate(CoinPredicate { predicate, .. })
            | Self::MessageCoinPredicate(MessageCoinPredicate { predicate, .. })
            | Self::MessageDataPredicate(MessageDataPredicate { predicate, .. })
                if predicate.len() > predicate_params.max_predicate_length as usize =>
            {
                Err(CheckError::InputPredicateLength { index })
            }

            Self::CoinPredicate(CoinPredicate { predicate_data, .. })
            | Self::MessageCoinPredicate(MessageCoinPredicate {
                predicate_data, ..
            })
            | Self::MessageDataPredicate(MessageDataPredicate {
                predicate_data, ..
            }) if predicate_data.len()
                > predicate_params.max_predicate_data_length as usize =>
            {
                Err(CheckError::InputPredicateDataLength { index })
            }

            Self::CoinSigned(CoinSigned { witness_index, .. })
            | Self::MessageCoinSigned(MessageCoinSigned { witness_index, .. })
            | Self::MessageDataSigned(MessageDataSigned { witness_index, .. })
                if *witness_index as usize >= witnesses.len() =>
            {
                Err(CheckError::InputWitnessIndexBounds { index })
            }

            // ∀ inputContract ∃! outputContract : outputContract.inputIndex =
            // inputContract.index
            Self::Contract { .. }
                if 1 != outputs
                    .iter()
                    .filter_map(|output| match output {
                        Output::Contract(output::contract::Contract {
                            input_index,
                            ..
                        }) if *input_index as usize == index => Some(()),
                        _ => None,
                    })
                    .count() =>
            {
                Err(CheckError::InputContractAssociatedOutputContract { index })
            }

            Self::MessageDataSigned(MessageDataSigned { data, .. })
            | Self::MessageDataPredicate(MessageDataPredicate { data, .. })
                if data.is_empty()
                    || data.len() > predicate_params.max_message_data_length as usize =>
            {
                Err(CheckError::InputMessageDataLength { index })
            }

            // TODO: If h is the block height the UTXO being spent was created,
            // transaction is  invalid if `blockheight() < h + maturity`.
            _ => Ok(()),
        }
    }
}

impl Output {
    /// Validate the output of the transaction.
    ///
    /// This function is stateful - meaning it might validate a transaction during VM
    /// initialization, but this transaction will no longer be valid in post-execution
    /// because the VM might mutate the message outputs, producing invalid
    /// transactions.
    pub fn check(&self, index: usize, inputs: &[Input]) -> Result<(), CheckError> {
        match self {
            Self::Contract(output::contract::Contract { input_index, .. }) => {
                match inputs.get(*input_index as usize) {
                    Some(Input::Contract { .. }) => Ok(()),
                    _ => Err(CheckError::OutputContractInputIndex { index }),
                }
            }

            _ => Ok(()),
        }
    }
}

/// Contains logic for stateless validations that don't result in any reusable metadata
/// such as spendable input balances or remaining gas. Primarily involves validating that
/// transaction fields are correctly formatted and signed.
pub trait FormatValidityChecks {
    /// Performs all stateless transaction validity checks. This includes the validity
    /// of fields according to rules in the specification and validity of signatures.
    fn check(
        &self,
        block_height: BlockHeight,
        consensus_params: &ConsensusParameters,
    ) -> Result<(), CheckError> {
        self.check_without_signatures(block_height, consensus_params)?;
        self.check_signatures(&consensus_params.chain_id())?;

        Ok(())
    }

    /// Validates that all required signatures are set in the transaction and that they
    /// are valid.
    fn check_signatures(&self, chain_id: &ChainId) -> Result<(), CheckError>;

    /// Validates the transactions according to rules from the specification:
    /// <https://github.com/FuelLabs/fuel-specs/blob/master/src/tx-format/transaction.md>
    fn check_without_signatures(
        &self,
        block_height: BlockHeight,
        consensus_params: &ConsensusParameters,
    ) -> Result<(), CheckError>;
}

impl FormatValidityChecks for Transaction {
    fn check_signatures(&self, chain_id: &ChainId) -> Result<(), CheckError> {
        match self {
            Transaction::Script(script) => script.check_signatures(chain_id),
            Transaction::Create(create) => create.check_signatures(chain_id),
            Transaction::Mint(mint) => mint.check_signatures(chain_id),
        }
    }

    fn check_without_signatures(
        &self,
        block_height: BlockHeight,
        consensus_params: &ConsensusParameters,
    ) -> Result<(), CheckError> {
        match self {
            Transaction::Script(script) => {
                script.check_without_signatures(block_height, consensus_params)
            }
            Transaction::Create(create) => {
                create.check_without_signatures(block_height, consensus_params)
            }
            Transaction::Mint(mint) => {
                mint.check_without_signatures(block_height, consensus_params)
            }
        }
    }
}

/// Validates the size of the transaction in bytes. Transactions cannot exceed
/// the total size specified by the transaction parameters. The size of a
/// transaction is calculated as the sum of the sizes of its static and dynamic
/// parts.
pub(crate) fn check_size<T>(tx: &T, tx_params: &TxParameters) -> Result<(), CheckError>
where
    T: canonical::Serialize,
{
    if tx.size() as u64 > tx_params.max_size {
        Err(CheckError::TransactionSizeLimitExceeded)?;
    }

    Ok(())
}

pub(crate) fn check_common_part<T>(
    tx: &T,
    block_height: BlockHeight,
    tx_params: &TxParameters,
    predicate_params: &PredicateParameters,
    base_asset_id: &AssetId,
) -> Result<(), CheckError>
where
    T: field::GasPrice
        + field::GasLimit
        + field::Maturity
        + field::Inputs
        + field::Outputs
        + field::Witnesses,
{
    if tx.gas_limit() > &tx_params.max_gas_per_tx {
        Err(CheckError::TransactionGasLimit)?
    }

    if tx.maturity() > &block_height {
        Err(CheckError::TransactionMaturity)?;
    }

    if tx.inputs().len() > tx_params.max_inputs as usize {
        Err(CheckError::TransactionInputsMax)?
    }

    if tx.outputs().len() > tx_params.max_outputs as usize {
        Err(CheckError::TransactionOutputsMax)?
    }

    if tx.witnesses().len() > tx_params.max_witnesses as usize {
        Err(CheckError::TransactionWitnessesMax)?
    }

    let any_spendable_input = tx.inputs().iter().find(|input| match input {
        Input::CoinSigned(_)
        | Input::CoinPredicate(_)
        | Input::MessageCoinSigned(_)
        | Input::MessageCoinPredicate(_) => true,
        Input::MessageDataSigned(_)
        | Input::MessageDataPredicate(_)
        | Input::Contract(_) => false,
    });

    if any_spendable_input.is_none() {
        Err(CheckError::NoSpendableInput)?
    }

    tx.input_asset_ids_unique(base_asset_id)
        .try_for_each(|input_asset_id| {
            // check for duplicate change outputs
            if tx
                .outputs()
                .iter()
                .filter_map(|output| match output {
                    Output::Change { asset_id, .. } if input_asset_id == asset_id => {
                        Some(())
                    }
                    Output::Change { asset_id, .. }
                        if asset_id != base_asset_id && input_asset_id == asset_id =>
                    {
                        Some(())
                    }
                    _ => None,
                })
                .count()
                > 1
            {
                return Err(CheckError::TransactionOutputChangeAssetIdDuplicated(
                    *input_asset_id,
                ))
            }

            Ok(())
        })?;

    // Check for duplicated input utxo id
    let duplicated_utxo_id = tx
        .inputs()
        .iter()
        .filter_map(|i| i.is_coin().then(|| i.utxo_id()).flatten());

    if let Some(utxo_id) = next_duplicate(duplicated_utxo_id).copied() {
        return Err(CheckError::DuplicateInputUtxoId { utxo_id })
    }

    // Check for duplicated input contract id
    let duplicated_contract_id = tx.inputs().iter().filter_map(Input::contract_id);

    if let Some(contract_id) = next_duplicate(duplicated_contract_id).copied() {
        return Err(CheckError::DuplicateInputContractId { contract_id })
    }

    // Check for duplicated input message id
    let duplicated_message_id = tx.inputs().iter().filter_map(Input::message_id);
    if let Some(message_id) = next_duplicate(duplicated_message_id) {
        return Err(CheckError::DuplicateMessageInputId { message_id })
    }

    // Validate the inputs without checking signature
    tx.inputs()
        .iter()
        .enumerate()
        .try_for_each(|(index, input)| {
            input.check_without_signature(
                index,
                tx.outputs(),
                tx.witnesses(),
                predicate_params,
            )
        })?;

    tx.outputs()
        .iter()
        .enumerate()
        .try_for_each(|(index, output)| {
            output.check(index, tx.inputs())?;

            if let Output::Change { asset_id, .. } = output {
                if !tx
                    .input_asset_ids(base_asset_id)
                    .any(|input_asset_id| input_asset_id == asset_id)
                {
                    return Err(CheckError::TransactionOutputChangeAssetIdNotFound(
                        *asset_id,
                    ))
                }
            }

            if let Output::Coin { asset_id, .. } = output {
                if !tx
                    .input_asset_ids(base_asset_id)
                    .any(|input_asset_id| input_asset_id == asset_id)
                {
                    return Err(CheckError::TransactionOutputCoinAssetIdNotFound(
                        *asset_id,
                    ))
                }
            }

            Ok(())
        })?;

    Ok(())
}

// TODO https://github.com/FuelLabs/fuel-tx/issues/148
pub(crate) fn next_duplicate<U>(iter: impl Iterator<Item = U>) -> Option<U>
where
    U: PartialEq + Ord + Copy + Hash,
{
    #[cfg(not(feature = "std"))]
    {
        iter.sorted()
            .as_slice()
            .windows(2)
            .filter_map(|u| (u[0] == u[1]).then(|| u[0]))
            .next()
    }

    #[cfg(feature = "std")]
    {
        iter.duplicates().next()
    }
}
