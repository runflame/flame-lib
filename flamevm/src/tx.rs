use bulletproofs::r1cs::R1CSProof;
use bulletproofs::PedersenGens;
use core::convert::TryFrom;
use curve25519_dalek::ristretto::CompressedRistretto;
use merkle::{Hash, MerkleItem, MerkleTree};
use merlin::Transcript;
use musig::Signature;
use readerwriter::{Encodable, ExactSizeEncodable, ReadError, Reader, WriteError, Writer};
use serde::{Deserialize, Serialize};

use crate::actor::{code_root, state_root, ActorID, ActorRegistry};
use crate::contract::{Contract, ContractID};
use crate::encoding::{write_admitted_value, write_int253};
use crate::errors::VMError;
use crate::message::Message;
use crate::prover::Prover;
use crate::script::ScriptBuilder;
use crate::verifier::Verifier;
use crate::vm::{BlockContext, DeferredSig, VM};
use crate::{Int253, Value};

/// Header metadata for the transaction
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct TxHeader {
    /// Version of the transaction
    pub version: u32,

    /// Timestamp before which tx is invalid, compatible with Bitcoin
    pub locktime: u32,
}

pub struct ExternalTx {
    /// Header metadata
    pub header: TxHeader,

    /// Script representing the transaction
    pub script: Vec<u8>,

    /// Aggregate TxID-bound signature, absent when the script records no
    /// `signtx` authorizations. Explicit `signcall` signatures live in script.
    pub signature: Option<Signature>,

    /// Constraint system proof for all the constraints
    pub proof: R1CSProof,
}

impl ExternalTx {
    /// Header committed by this transaction.
    pub fn header(&self) -> TxHeader {
        self.header
    }

    /// Canonical VM bytecode carried by this transaction.
    pub fn script(&self) -> &[u8] {
        &self.script
    }

    /// Optional aggregate signature bytes used by the network envelope.
    pub fn signature_bytes(&self) -> Option<[u8; 64]> {
        self.signature.map(|signature| signature.to_bytes())
    }

    /// R1CS proof bytes used by block witness commitments.
    pub fn proof_bytes(&self) -> Vec<u8> {
        self.proof.to_bytes()
    }

    /// Decodes one canonical transaction envelope under caller-provided
    /// network bounds. Execution policy remains in [`Self::verify`].
    pub fn decode_bounded(
        reader: &mut impl Reader,
        expected_version: u32,
        max_script_bytes: usize,
        max_proof_bytes: usize,
    ) -> Result<Self, ReadError> {
        let header = TxHeader {
            version: reader.read_u32()?,
            locktime: reader.read_u32()?,
        };
        if expected_version != 1 || header.version != expected_version {
            return Err(ReadError::InvalidFormat);
        }
        let script_len =
            usize::try_from(reader.read_u64()?).map_err(|_| ReadError::InvalidFormat)?;
        if script_len > max_script_bytes {
            return Err(ReadError::InvalidFormat);
        }
        let script = reader.read_bytes(script_len)?;
        let signature = match reader.read_u8()? {
            0 => None,
            1 => Some(
                Signature::from_bytes(reader.read_u8x64()?)
                    .map_err(|_| ReadError::InvalidFormat)?,
            ),
            _ => return Err(ReadError::InvalidFormat),
        };
        let proof_len =
            usize::try_from(reader.read_u64()?).map_err(|_| ReadError::InvalidFormat)?;
        if proof_len > max_proof_bytes {
            return Err(ReadError::InvalidFormat);
        }
        let proof = R1CSProof::from_bytes(&reader.read_bytes(proof_len)?)
            .map_err(|_| ReadError::InvalidFormat)?;
        Ok(Self {
            header,
            script,
            signature,
            proof,
        })
    }

    /// Decodes one complete transaction and rejects trailing bytes.
    pub fn from_bytes_bounded(
        bytes: &[u8],
        expected_version: u32,
        max_script_bytes: usize,
        max_proof_bytes: usize,
    ) -> Result<Self, ReadError> {
        let mut reader = bytes;
        reader.read_all(|reader| {
            Self::decode_bounded(reader, expected_version, max_script_bytes, max_proof_bytes)
        })
    }

    /// Lifecycle step 3: verify the signed transaction — run the opaque
    /// program, check the proof and the aggregate signature — and return
    /// its effects. Bulletproof generators are managed inside the crate.
    pub fn verify(&self, limits: Limits) -> Result<TxLog, VMError> {
        self.verify_with_metrics(limits).map(|(log, _)| log)
    }

    /// Verifies the transaction and returns both its effects and actual
    /// execution counters for consensus admission.
    pub fn verify_with_metrics(&self, limits: Limits) -> Result<(TxLog, TxMetrics), VMError> {
        let pc_gens = PedersenGens::default();
        let result = Verifier::verify(
            &pc_gens,
            self.script.clone(),
            &self.proof,
            self.header,
            limits.gas,
            self.signature,
        )?;
        Ok((
            TxLog(result.txlog),
            TxMetrics {
                gas_used: result.gas_used,
                total_fee: result.total_fee,
                multiplications: result.multiplications,
            },
        ))
    }
}

impl Encodable for ExternalTx {
    fn encode(&self, writer: &mut impl Writer) -> Result<(), WriteError> {
        writer.write_u32(b"external_tx.version", self.header.version)?;
        writer.write_u32(b"external_tx.locktime", self.header.locktime)?;
        writer.write_u64(b"external_tx.script_len", self.script.len() as u64)?;
        writer.write(b"external_tx.script", &self.script)?;
        match self.signature {
            Some(signature) => {
                writer.write_u8(b"external_tx.signature_present", 1)?;
                writer.write(b"external_tx.signature", &signature.to_bytes())?;
            }
            None => writer.write_u8(b"external_tx.signature_present", 0)?,
        }
        let proof = self.proof.to_bytes();
        writer.write_u64(b"external_tx.proof_len", proof.len() as u64)?;
        writer.write(b"external_tx.proof", &proof)
    }
}

impl ExactSizeEncodable for ExternalTx {
    fn encoded_size(&self) -> usize {
        25usize
            .saturating_add(usize::from(self.signature.is_some()) * 64)
            .saturating_add(self.script.len())
            .saturating_add(self.proof.to_bytes().len())
    }
}

/// Resource limits for one transaction's execution.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Compute (gas) budget.
    pub gas: u64,
}

/// Ordered transaction effects — the canonical change set a node
/// applies to its state. The [`TxID`] is the merkle root over these.
pub struct TxLog(Vec<TxEntry>);

/// For the node layer (and tests): wrap a re-derived effect list.
/// The crate itself only ever produces TxLogs by execution.
impl From<Vec<TxEntry>> for TxLog {
    fn from(entries: Vec<TxEntry>) -> Self {
        TxLog(entries)
    }
}

impl TxLog {
    /// Canonical transaction id (merkle root over the effect list).
    pub fn txid(&self) -> TxID {
        TxID::from_log(&self.0)
    }
    /// The effect entries in canonical order.
    pub fn entries(&self) -> &[TxEntry] {
        &self.0
    }
    /// Iterates the effect entries.
    pub fn iter(&self) -> std::slice::Iter<'_, TxEntry> {
        self.0.iter()
    }

    /// Sum of gas grants on messages emitted directly by this execution.
    /// Descendant messages are charged to their own derived logs.
    pub fn direct_send_gas(&self) -> Option<u64> {
        self.0.iter().try_fold(0u64, |sum, entry| match entry {
            TxEntry::Send(message) => sum.checked_add(message.gas),
            _ => Some(sum),
        })
    }

    /// Consumes the log and returns its ordered effects. Consensus code uses
    /// this to move linear contracts and messages into the block transition
    /// without cloning bearer values.
    pub fn into_entries(self) -> Vec<TxEntry> {
        self.0
    }
}

/// Actual resource counters surfaced for consensus admission and diagnostics.
/// They are re-derived by execution and are not part of the [`TxID`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxMetrics {
    pub gas_used: u64,
    pub total_fee: u64,
    pub multiplications: usize,
}

/// What the sender must aggregate-sign before broadcast. Build the
/// signature with `musig::Signature::sign_multi(keys, items, t)` where
/// `t` is a `b"flamevm.signtx"` transcript with `txid` appended under
/// `b"txid"`; pass the result to [`UnsignedTx::sign`].
pub struct SigningInstructions {
    pub txid: TxID,
    /// The tx-bound `(verification_key, contract_id)` authorizations, in
    /// `signtx` order — the multi-message context to sign.
    pub items: Vec<(CompressedRistretto, ContractID)>,
}

/// A built-but-unsigned external transaction (lifecycle step 1→2).
pub struct UnsignedTx {
    header: TxHeader,
    script: Vec<u8>,
    proof: R1CSProof,
    log: TxLog,
    metrics: TxMetrics,
    txbound_items: Vec<(CompressedRistretto, ContractID)>,
}

impl UnsignedTx {
    /// The transaction effects.
    pub fn log(&self) -> &TxLog {
        &self.log
    }
    /// Resource counters (measurement only).
    pub fn metrics(&self) -> TxMetrics {
        self.metrics
    }
    /// The keys + txid the sender signs over (lifecycle step 2 input).
    pub fn signing_instructions(&self) -> SigningInstructions {
        SigningInstructions {
            txid: self.log.txid(),
            items: self.txbound_items.clone(),
        }
    }
    /// Attaches the aggregate signature → broadcastable [`ExternalTx`].
    pub fn sign(self, signature: Signature) -> ExternalTx {
        ExternalTx {
            header: self.header,
            script: self.script,
            signature: Some(signature),
            proof: self.proof,
        }
    }

    /// Finalizes a transaction that recorded no `signtx` authorizations.
    pub fn without_signature(self) -> Result<ExternalTx, VMError> {
        if !self.txbound_items.is_empty() {
            return Err(VMError::MissingTxBoundSignature);
        }
        Ok(ExternalTx {
            header: self.header,
            script: self.script,
            signature: None,
            proof: self.proof,
        })
    }
}

impl ScriptBuilder {
    /// Lifecycle step 1: build an unsigned external transaction by
    /// running the witness-bearing program through the prover.
    /// Bulletproof generators are managed inside the crate.
    pub fn build_tx(self, header: TxHeader, limits: Limits) -> Result<UnsignedTx, VMError> {
        let pc_gens = PedersenGens::default();
        let result = Prover::prove(&pc_gens, self, header, limits.gas)?;
        let txbound_items = result
            .deferred_sigs
            .iter()
            .filter_map(|s| match s {
                DeferredSig::TxBound {
                    verification_key,
                    contract_id,
                } => Some((*verification_key, *contract_id)),
                DeferredSig::Explicit { .. } => None,
            })
            .collect();
        Ok(UnsignedTx {
            header,
            script: result.bytecode,
            proof: result.proof.expect("prover always sets the proof"),
            metrics: TxMetrics {
                gas_used: result.gas_used,
                total_fee: result.total_fee,
                multiplications: result.multiplications,
            },
            txbound_items,
            log: TxLog(result.txlog),
        })
    }
}

/// An internal transaction's outcome (lifecycle step 4): the effects
/// to apply + resource counters. Produced by [`Message::execute_tx`].
pub struct InternalTx {
    log: TxLog,
    metrics: TxMetrics,
}

impl InternalTx {
    /// The effects to apply to chain state.
    pub fn log(&self) -> &TxLog {
        &self.log
    }
    /// Resource counters (measurement only).
    pub fn metrics(&self) -> TxMetrics {
        self.metrics
    }

    /// Consumes the result and returns its effect log.
    pub fn into_log(self) -> TxLog {
        self.log
    }
}

impl Message {
    /// Runs one derived internal transaction directly against the chain's
    /// checkpointed actor registry. This avoids cloning and partially replaying
    /// consensus state; FlameVM commits or rolls back the registry atomically.
    pub fn execute_tx(
        self,
        registry: &mut dyn ActorRegistry,
        block: &BlockContext,
    ) -> Result<InternalTx, VMError> {
        // Internal-tx header: fixed default for now — its source is part
        // of the block envelope design.
        let header = TxHeader {
            version: 1,
            locktime: 0,
        };
        let result = VM::execute_internal(header, self, registry, block)?;
        Ok(InternalTx {
            log: TxLog(result.txlog),
            metrics: TxMetrics {
                gas_used: result.gas_used,
                total_fee: result.total_fee,
                multiplications: result.multiplications,
            },
        })
    }
}

/// Transaction ID is a unique 32-byte identifier of a transaction effects represented by `TxLog`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxID(pub Hash);

/// Entry in a transaction log. All entries are hashed into a [transaction ID](TxID).
///
/// `Clone`/`Serialize`/`Deserialize` are still withheld (the linear
/// `Contract`/`Token` payloads don't participate); downstream code wanting
/// those should hash entries to bytes first.
#[derive(Debug)]
pub enum TxEntry {
    /// Tx header — bound at run start as the first txlog entry so
    /// `version` and `locktime` participate in `TxID::from_log`.
    Header(TxHeader),

    /// Plain data entry created by `log` instruction. Contains arbitrary binary string.
    Data(Vec<u8>),

    /// Input: a consumed contract's identity. Emitted by `input` (external-only).
    /// Commits the contract that the transaction has consumed without re-storing
    /// the payload — the existence of the contract is independently asserted by
    /// the Utreexo proof outside the VM.
    Input(ContractID),

    /// Receive: the MessageID consumed by an internal transaction. Emitted
    /// by `VM::execute_internal` as the first effect after `Header`,
    /// committing the originating `Send`'s anchor into the Internal TxID
    /// merkle root. Without this entry, the Internal TxID would not
    /// commit to its triggering Send (the originating `TxEntry::Send`
    /// lives in the *external* tx's log, hence in External TxID only).
    /// Symmetric with `Input` for external transactions: both are
    /// "what triggered me" effects emitted before the body runs.
    Receive([u8; 32]),

    /// Successful first delivery to a constructor-form actor. Carries the
    /// canonical actor id and full constructor code so state-machine replay
    /// does not need the original Message. The initial state is the canonical
    /// empty state.
    ActorDeploy { actor: ActorID, code: Vec<u8> },

    /// Output: a newly sealed contract, emitted by the `output` opcode.
    Output(Contract),

    /// Cleartext issuance (emitted by `op_issuepub`). Carries the
    /// cleartext `(qty, flv)` pair as `Int253`s — public on the wire,
    /// directly auditable. Flavor is `flavor_from_actor(actor, tag)`
    /// for the actor that ran `issuepub`.
    IssuePub(Int253, Int253),

    /// Confidential issuance (emitted by `op_issuepriv`). Carries
    /// `(qty_point, flv_point)` — the live Pedersen commitment to the
    /// qty and the unblinded commitment to the flavor scalar. Flavor
    /// is `flavor_from_predicate(predicate, tag)` for the predicate
    /// whose `ContractOpen` frame ran `issuepriv`. Soundness of the qty
    /// commitment + 64-bit range proof is established through the
    /// constraint system.
    IssuePriv(CompressedRistretto, CompressedRistretto),

    /// Retirement: an asset value has been *destroyed* from circulation.
    /// Same `(qty_point, flv_point)` shape as `Issue`. Cleartext or
    /// encrypted symmetrically.
    Retire(CompressedRistretto, CompressedRistretto),

    /// Fee: a transaction fee of `qty` flames recorded by `op_fee`.
    /// Carried as a bare `u64` (no commitment) — the cleartext
    /// branch is currently the only defined fee shape; the matching
    /// debt half is the `WideToken` returned to the stack.
    /// Aggregated by `VM::total_fee` (a `CheckedFee`) into the
    /// eventual `TxResult.total_fee`.
    Fee(u64),

    /// Actor-state mutation recorded by `op_save`. Carries the
    /// actor's identity and the **full** post-save state Value —
    /// symmetric with `Output(Contract)` which carries the full Contract.
    /// The state machine consumes this entry by replacing the
    /// actor's stored state with `state`; no re-execution of the
    /// script needed. See docs/flamevm.md §"Design"; TxLog records effects, not
    /// control flow".
    ///
    /// The MerkleItem encoding hashes `(actor.to_hash(),
    /// state_root(&state))` — i.e. the merkle leaf commits to the
    /// canonical state root, not the full bytes, just as
    /// `Output(Contract)`'s leaf commits to `contract.id()`.
    ActorSave { actor: ActorID, state: Value },

    /// Actor-code replacement recorded by `setcode`. Carries the full
    /// new code blob for state-machine replay; the merkle leaf commits
    /// to `(actor.to_hash(), code_root(&code))`. Symmetric with
    /// `ActorSave`. See ADR 0018.
    SetCode { actor: ActorID, code: Vec<u8> },

    /// Outbound asynchronous message scheduled by `op_send`. Carries
    /// the full [`Message`](Message) — its `anchor` is
    /// the `left` half of a split of `last_anchor` at the send site,
    /// and its `id()` is the canonical MessageID (deterministic at
    /// broadcast time, identifies the future internal-tx delivery).
    ///
    /// The block builder reads `TxEntry::Send` entries directly from
    /// the TxLog — there is no separate "sends" queue — and feeds the
    /// embedded `Message` straight into `VM::execute_internal`.
    /// Symmetric with `TxEntry::Output(Contract)`: each effect that owns
    /// an addressable artifact embeds the artifact itself.
    Send(Message),

    /// Persistent storage purchased by an actor. Replay recomputes the quote
    /// from the ordered storage-pool state and requires this allocation and
    /// fee to match exactly.
    StoragePurchase {
        actor: ActorID,
        bytes: u64,
        expiry_height: u64,
        fee_sparks: Int253,
    },

    /// Deterministic removal of an actor, either by explicit state
    /// dismantling or by the blockchain's block-boundary expiry process.
    ActorDestroy { actor: ActorID },
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    fn hex_bytes(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => panic!("invalid test vector"),
                };
                digit(pair[0]) << 4 | digit(pair[1])
            })
            .collect()
    }

    #[test]
    fn canonical_external_tx_vector_and_bounds() {
        const VECTOR: &str = "010000000200000000000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a101000000000000007e5de4349c5b87f2e1003095aff2e310801e2504b706bc6c062076eee49f90366625b75748908fb2492dd909a6d1428001dfdd201a0a7fae70911cf29112c8319e9d0eba4ca7fe137d5f8026614ab8736204ea46c213d9a20d0d663aa3e8ff1676fcd93dc1cba92d2f820b5b8ae5c99bacce0610dc799f050d1dec5effd5cb6c96950b0ad392e7414252008e6ff97d385437f30c74f106ae586522db4a9d73241ca0ed4f24798b31981e98e96bc121852a567728380ca00d12ee8556c220c13c3ed16d35fca58a3a3773120657b5b49cac1830a472bd083c51f4012ab7de25450a4544cdee6b7577d97a9c3e5a3267da4e13e2ef36a65ce83697cc498f00d005000000000000000000000000000000000000000000000000000000000000000027e4219ec9efc32f50b4b1c8766037a812d135363cbaa38be71527de967eb20839057e9d2324d2932cba8c6a646bb2b9f09661cd1ef8977bbd1df4813803e4040000000000000000000000000000000000000000000000000000000000000000ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010";
        let bytes = hex_bytes(VECTOR);
        let tx = ExternalTx::from_bytes_bounded(&bytes, 1, 0, 417).unwrap();
        assert_eq!(tx.encode_to_vec(), bytes);
        assert_eq!(tx.encoded_size(), bytes.len());

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&trailing, 1, 0, 417),
            Err(ReadError::TrailingBytes)
        ));
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 2, 0, 417),
            Err(ReadError::InvalidFormat)
        ));
        let mut unknown_version = bytes.clone();
        unknown_version[0] = 2;
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&unknown_version, 2, 0, 417),
            Err(ReadError::InvalidFormat)
        ));
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 1, 0, 416),
            Err(ReadError::InvalidFormat)
        ));
    }

    #[test]
    fn transaction_without_txbound_authorization_omits_signature() {
        let limits = Limits { gas: 10_000 };
        let tx = ScriptBuilder::new()
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                limits,
            )
            .unwrap()
            .without_signature()
            .unwrap();
        assert!(tx.signature_bytes().is_none());
        let (_, metrics) = tx.verify_with_metrics(limits).unwrap();
        assert!(metrics.gas_used > 0);
        assert_eq!(metrics.multiplications, 0);
    }

    #[test]
    fn multiplication_metrics_include_randomized_constraints() {
        let limits = Limits { gas: 100_000 };
        let unsigned = ScriptBuilder::new()
            .alloc(Some(Int253::ONE))
            .alloc(Some(Int253::from(2u64)))
            .eq()
            .not()
            .verify()
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                limits,
            )
            .unwrap();
        assert_eq!(unsigned.metrics().multiplications, 3);
        let tx = unsigned.without_signature().unwrap();
        assert_eq!(tx.verify_with_metrics(limits).unwrap().1.multiplications, 3);
    }
}

impl TxID {
    /// Canonical transaction identity: the merkle root over the txlog
    /// (header + effect list).
    pub fn from_log(txlog: &[TxEntry]) -> Self {
        TxID(MerkleTree::root(b"flamevm.txid", txlog))
    }
}

impl MerkleItem for TxEntry {
    fn commit(&self, t: &mut Transcript) {
        match self {
            TxEntry::Header(h) => {
                // Absorb version and locktime as little-endian u32 —
                // matches the wire format (docs/flamevm.md).
                t.append_message(b"tx.version", &h.version.to_le_bytes());
                t.append_message(b"tx.locktime", &h.locktime.to_le_bytes());
            }
            TxEntry::Data(bytes) => {
                t.append_message(b"data", bytes);
            }
            TxEntry::Input(contract_id) => {
                t.append_message(b"input", contract_id);
            }
            TxEntry::Receive(send_id) => {
                t.append_message(b"receive.send_id", send_id);
            }
            TxEntry::ActorDeploy { actor, code } => {
                t.append_message(b"deploy.actor", &actor.to_hash());
                t.append_message(b"deploy.code_root", &code_root(code));
            }
            TxEntry::Output(contract) => {
                // Bind to the contract's canonical 32-byte identity hash.
                // Contract::id() already absorbs predicate / anchor /
                // payload bytes via Merlin.
                let id = contract.id();
                t.append_message(b"output", &id);
            }
            TxEntry::IssuePub(qty, flv) => {
                t.append_message(b"issuepub.qty", &qty.to_bytes());
                t.append_message(b"issuepub.flv", &flv.to_bytes());
            }
            TxEntry::IssuePriv(qty_pt, flv_pt) => {
                t.append_message(b"issuepriv.qty", qty_pt.as_bytes());
                t.append_message(b"issuepriv.flv", flv_pt.as_bytes());
            }
            TxEntry::Retire(qty_pt, flv_pt) => {
                t.append_message(b"retire.qty", qty_pt.as_bytes());
                t.append_message(b"retire.flv", flv_pt.as_bytes());
            }
            TxEntry::Fee(qty) => {
                // Little-endian u64, per docs/flamevm.md "Encoding:
                // little-endian everywhere". Domain tag distinguishes
                // this from any other 8-byte append.
                t.append_message(b"fee.qty", &qty.to_le_bytes());
            }
            TxEntry::ActorSave { actor, state } => {
                // Bind every actor-state mutation into the TxID merkle
                // root: actor identity (canonical 32-byte hash, not
                // variant-tagged wire form) + state root. State bytes
                // ride in the entry itself; the merkle leaf commits
                // only to the root, matching Output's Contract-as-id
                // pattern.
                t.append_message(b"save.actor", &actor.to_hash());
                t.append_message(b"save.post_state_root", &state_root(state));
            }
            TxEntry::SetCode { actor, code } => {
                t.append_message(b"setcode.actor", &actor.to_hash());
                t.append_message(b"setcode.code_root", &code_root(code));
            }
            TxEntry::Send(msg) => {
                // Bind to the send's canonical 32-byte MessageID hash,
                // analogous to `Output(Contract)` committing only to
                // `contract.id()`. `Message::id()` absorbs the message's
                // canonical wire encoding under domain
                // `flamevm.message.id`, so this single leaf commits to
                // every parameter the future internal tx will be
                // delivered with.
                t.append_message(b"send", msg.id().as_bytes());
            }
            TxEntry::StoragePurchase {
                actor,
                bytes,
                expiry_height,
                fee_sparks,
            } => {
                t.append_message(b"storage.actor", &actor.to_hash());
                t.append_message(b"storage.bytes", &bytes.to_le_bytes());
                t.append_message(b"storage.expiry", &expiry_height.to_le_bytes());
                t.append_message(b"storage.fee_sparks", &fee_sparks.to_bytes());
            }
            TxEntry::ActorDestroy { actor } => {
                t.append_message(b"destroy.actor", &actor.to_hash());
            }
        }
    }
}

impl TxEntry {
    /// Stable wire tags (spec §TxLog transport). New effects append tags
    /// without renumbering existing entries. Encode-only: the TxLog is
    /// re-derived by execution, never decoded from the wire by this crate.
    pub const TAG_HEADER: u8 = 0;
    pub const TAG_DATA: u8 = 1;
    pub const TAG_INPUT: u8 = 2;
    pub const TAG_RECEIVE: u8 = 3;
    pub const TAG_OUTPUT: u8 = 4;
    pub const TAG_ISSUE_PUB: u8 = 5;
    pub const TAG_ISSUE_PRIV: u8 = 6;
    pub const TAG_RETIRE: u8 = 7;
    pub const TAG_FEE: u8 = 8;
    pub const TAG_ACTOR_SAVE: u8 = 9;
    pub const TAG_SET_CODE: u8 = 10;
    pub const TAG_SEND: u8 = 11;
    pub const TAG_STORAGE_PURCHASE: u8 = 12;
    pub const TAG_ACTOR_DESTROY: u8 = 13;
    pub const TAG_ACTOR_DEPLOY: u8 = 14;
}

/// Canonical wire serialization of one effect: a tag byte followed by
/// the variant's fields, each in its existing canonical form (reusing
/// `Contract`/`Message`/`ActorID` encoders and `write_value`/`write_int253`
/// — never a parallel re-implementation, per spec §TxLog transport).
/// All integers little-endian (ADR 0006); byte blobs are u64-LE
/// length-prefixed (matching `Message` payload / `ActorID` ctor style).
impl Encodable for TxEntry {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        match self {
            TxEntry::Header(h) => {
                w.write_u8(b"txentry.tag", Self::TAG_HEADER)?;
                w.write(b"tx.version", &h.version.to_le_bytes())?;
                w.write(b"tx.locktime", &h.locktime.to_le_bytes())
            }
            TxEntry::Data(bytes) => {
                w.write_u8(b"txentry.tag", Self::TAG_DATA)?;
                w.write_u64(b"data.len", bytes.len() as u64)?;
                w.write(b"data.bytes", bytes)
            }
            TxEntry::Input(contract_id) => {
                w.write_u8(b"txentry.tag", Self::TAG_INPUT)?;
                w.write(b"input.contract_id", contract_id)
            }
            TxEntry::Receive(send_id) => {
                w.write_u8(b"txentry.tag", Self::TAG_RECEIVE)?;
                w.write(b"receive.send_id", send_id)
            }
            TxEntry::ActorDeploy { actor, code } => {
                w.write_u8(b"txentry.tag", Self::TAG_ACTOR_DEPLOY)?;
                actor.to_canonical().encode(w)?;
                w.write_u64(b"deploy.len", code.len() as u64)?;
                w.write(b"deploy.bytes", code)
            }
            TxEntry::Output(contract) => {
                w.write_u8(b"txentry.tag", Self::TAG_OUTPUT)?;
                contract.encode(w)
            }
            TxEntry::IssuePub(qty, flv) => {
                w.write_u8(b"txentry.tag", Self::TAG_ISSUE_PUB)?;
                write_int253(w, qty)?;
                write_int253(w, flv)
            }
            TxEntry::IssuePriv(qty_pt, flv_pt) => {
                w.write_u8(b"txentry.tag", Self::TAG_ISSUE_PRIV)?;
                w.write(b"issuepriv.qty", qty_pt.as_bytes())?;
                w.write(b"issuepriv.flv", flv_pt.as_bytes())
            }
            TxEntry::Retire(qty_pt, flv_pt) => {
                w.write_u8(b"txentry.tag", Self::TAG_RETIRE)?;
                w.write(b"retire.qty", qty_pt.as_bytes())?;
                w.write(b"retire.flv", flv_pt.as_bytes())
            }
            TxEntry::Fee(qty) => {
                w.write_u8(b"txentry.tag", Self::TAG_FEE)?;
                w.write_u64(b"fee.qty", *qty)
            }
            TxEntry::ActorSave { actor, state } => {
                w.write_u8(b"txentry.tag", Self::TAG_ACTOR_SAVE)?;
                actor.to_canonical().encode(w)?;
                write_admitted_value(w, state)
            }
            TxEntry::SetCode { actor, code } => {
                w.write_u8(b"txentry.tag", Self::TAG_SET_CODE)?;
                actor.to_canonical().encode(w)?;
                w.write_u64(b"setcode.len", code.len() as u64)?;
                w.write(b"setcode.bytes", code)
            }
            TxEntry::Send(msg) => {
                w.write_u8(b"txentry.tag", Self::TAG_SEND)?;
                msg.encode(w)
            }
            TxEntry::StoragePurchase {
                actor,
                bytes,
                expiry_height,
                fee_sparks,
            } => {
                w.write_u8(b"txentry.tag", Self::TAG_STORAGE_PURCHASE)?;
                actor.to_canonical().encode(w)?;
                w.write_u64(b"storage.bytes", *bytes)?;
                w.write_u64(b"storage.expiry", *expiry_height)?;
                write_int253(w, fee_sparks)
            }
            TxEntry::ActorDestroy { actor } => {
                w.write_u8(b"txentry.tag", Self::TAG_ACTOR_DESTROY)?;
                actor.to_canonical().encode(w)
            }
        }
    }
}

/// Canonical wire serialization of a whole log: u64-LE entry count
/// followed by each entry's encoding.
impl Encodable for TxLog {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        w.write_u64(b"txlog.len", self.0.len() as u64)?;
        for entry in &self.0 {
            entry.encode(w)?;
        }
        Ok(())
    }
}
