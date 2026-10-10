//! A transfer, from served bytes to signed bytes, in one call.

use flamekd::ReceivingAddress;
use flamepayments::{InputSpec, OutputSpec, SpendAccount};
use flamevm::{Limits, TxHeader};
use rand::rngs::OsRng;
use zeroize::Zeroizing;

use crate::contract;
use crate::convert;
use crate::error::FlameError;
use crate::keys::KeyPath;

/// The transaction version every chain today accepts.
const TX_VERSION: u32 = 1;

/// The secret behind a confidential output: what its recipient needs in
/// order to spend it. Scalars are 32 canonical little-endian bytes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Opening {
    pub qty: u64,
    pub flavor: Vec<u8>,
    pub qty_blinding: Vec<u8>,
    pub flavor_blinding: Vec<u8>,
}

/// One contract being spent, exactly as the indexer served it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct TransferInput {
    /// The contract's published bytes.
    pub contract: Vec<u8>,
    /// Its Utreexo membership proof, valid at the tip the transfer targets.
    pub proof: Vec<u8>,
    /// The path of the key it is locked to.
    pub path: KeyPath,
    /// Required for a confidential contract, refused for a cleartext one.
    pub opening: Option<Opening>,
}

/// One contract being created.
#[derive(Clone, Debug, uniffi::Record)]
pub struct TransferOutput {
    /// The recipient's bech32f address, for the wallet's network; for
    /// change, [`crate::Wallet::address`] on the change branch.
    pub address: String,
    pub qty: u64,
    /// `None` for flames.
    pub flavor: Option<Vec<u8>>,
    /// Sealed into the output's note with the amount; only the recipient
    /// reads it. At most 4006 bytes; empty for none.
    pub memo: Vec<u8>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct TransferRequest {
    pub inputs: Vec<TransferInput>,
    pub outputs: Vec<TransferOutput>,
    /// In sparks. Inputs must equal outputs plus fee, per flavor.
    pub fee: u64,
    /// The gas budget; the node refuses more than its block limit allows.
    pub gas: u64,
    /// `0` for none; otherwise as in Bitcoin.
    pub locktime: u32,
}

/// One contract the transfer creates.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CreatedOutput {
    pub contract_id: Vec<u8>,
    /// Its published bytes, as an indexer will serve them once confirmed.
    pub contract: Vec<u8>,
    /// The encrypted note that follows it on chain, as an indexer's scan
    /// serves it. Its recipient opens it with [`crate::Wallet::open_note`];
    /// so does this wallet, for its change.
    pub note: Vec<u8>,
}

/// A signed transfer, ready to publish.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Transfer {
    /// The 32-byte transaction id.
    pub txid: Vec<u8>,
    /// `BlockTx::to_bytes`: what a node's `submit_tx` takes.
    pub block_tx: Vec<u8>,
    /// In the order they are published, which is not request order: the
    /// builder sorts them so that position says nothing about which one is
    /// the change. Match one to its request by its predicate.
    pub outputs: Vec<CreatedOutput>,
}

impl Opening {
    pub(crate) fn to_wallet(&self) -> Result<flamepayments::Opening, FlameError> {
        Ok(flamepayments::Opening {
            qty: self.qty,
            flv: convert::flavor(Some(&self.flavor))?,
            qty_blinding: convert::blinding(&self.qty_blinding)?,
            flv_blinding: convert::blinding(&self.flavor_blinding)?,
        })
    }

    pub(crate) fn from_wallet(opening: &flamepayments::Opening) -> Opening {
        Opening {
            qty: opening.qty,
            flavor: opening.flv.to_bytes().to_vec(),
            qty_blinding: opening.qty_blinding.to_bytes().to_vec(),
            flavor_blinding: opening.flv_blinding.to_bytes().to_vec(),
        }
    }
}

pub(crate) fn build(
    account: &SpendAccount,
    request: TransferRequest,
) -> Result<Transfer, FlameError> {
    let mut specs = Vec::with_capacity(request.inputs.len());
    let mut keys = Zeroizing::new(Vec::with_capacity(request.inputs.len()));
    for (position, input) in request.inputs.iter().enumerate() {
        let published = contract::decode(&input.contract)?;
        let proof = flamechain::codec::proof_from_bytes(&input.proof)
            .map_err(|error| FlameError::bytes("proof", format!("{error:?}")))?;
        let KeyPath { branch, index } = input.path;
        // A wrong path signs with a key the predicate never names; the
        // transaction would build, prove and sign, and every node refuse it.
        if published.predicate.to_point() != account.predicate_at(branch, index)?.to_point() {
            return Err(FlameError::KeyMismatch {
                input: position as u32,
            });
        }
        let key = account.spending_key_at(branch, index)?;
        let spec = match &input.opening {
            None => InputSpec::clear(published, proof, key),
            Some(opening) => InputSpec::confidential(&published, &opening.to_wallet()?, proof, key),
        }
        .map_err(FlameError::transfer)?;
        specs.push(spec);
        keys.push(key);
    }

    let network = account.network();
    let outputs = request
        .outputs
        .iter()
        .map(|output| {
            Ok(OutputSpec {
                address: ReceivingAddress::from_bech32(&output.address, network).map_err(
                    |error| FlameError::InvalidAddress {
                        reason: error.to_string(),
                    },
                )?,
                qty: output.qty,
                flv: convert::flavor(output.flavor.as_deref())?,
                memo: output.memo.clone(),
            })
        })
        .collect::<Result<Vec<_>, FlameError>>()?;

    let header = TxHeader {
        version: TX_VERSION,
        locktime: request.locktime,
    };
    let limits = Limits { gas: request.gas };
    let unsigned =
        flamepayments::build_transfer(&specs, &outputs, request.fee, header, limits, &mut OsRng)
            .map_err(FlameError::transfer)?;

    let created = flamepayments::outputs_with_notes(unsigned.log())
        .into_iter()
        .map(|(contract, note)| {
            Ok(CreatedOutput {
                contract_id: contract.id().to_vec(),
                contract: flamechain::codec::contract_bytes(contract)
                    .map_err(FlameError::transfer)?,
                note: note
                    .expect("the builder writes a note after every output")
                    .to_vec(),
            })
        })
        .collect::<Result<Vec<_>, FlameError>>()?;
    let txid = unsigned.txid();

    let proofs = specs.iter().map(|spec| spec.proof().clone()).collect();
    let signed = flamepayments::sign(unsigned, &keys).map_err(FlameError::transfer)?;
    let block_tx = flamepayments::block_tx(signed, limits, proofs)
        .to_bytes()
        .map_err(FlameError::transfer)?;

    Ok(Transfer {
        txid: txid.0.to_vec(),
        block_tx,
        outputs: created,
    })
}
