use bulletproofs::r1cs::R1CSProof;
use bulletproofs::PedersenGens;
use curve25519_dalek::ristretto::CompressedRistretto;
use merkle::{Hash, MerkleItem, MerkleTree};
use merlin::Transcript;
use musig::Signature;
use serde::{Deserialize, Serialize};

use crate::actor::{ActorRegistry, MemRegistry};
use crate::errors::VMError;
use crate::program::Program;
use crate::prover::Prover;
use crate::send::Message;
use crate::verifier::Verifier;
use crate::vm::{BlockContext, DeferredSig, VM};

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

    /// Aggregated signature of the txid
    pub signature: Signature,

    /// Constraint system proof for all the constraints
    pub proof: R1CSProof,
}

impl ExternalTx {
    /// Lifecycle step 3: verify the signed transaction — run the opaque
    /// program, check the proof and the aggregate signature — and return
    /// its effects. Bulletproof generators are managed inside the crate.
    pub fn verify(&self, limits: Limits) -> Result<TxLog, VMError> {
        let pc_gens = PedersenGens::default();
        let result = Verifier::verify(
            &pc_gens,
            self.script.clone(),
            &self.proof,
            self.header,
            limits.gas,
            limits.mem,
            Some(self.signature.clone()),
        )?;
        Ok(TxLog(result.txlog))
    }
}

/// Resource limits for one transaction's execution.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Compute (gas) budget.
    pub gas: u64,
    /// Transient-memory cap (vbytes) for the outermost call.
    pub mem: u64,
}

/// Ordered transaction effects — the canonical change set a node
/// applies to its state. The [`TxID`] is the merkle root over these.
pub struct TxLog(Vec<TxEntry>);

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
}

/// Resource counters surfaced for measurement/debugging. Not part of
/// the [`TxID`].
#[derive(Clone, Copy, Debug)]
pub struct TxMetrics {
    pub gas_used: u64,
    pub total_fee: u64,
    pub vbytes_used: u64,
}

/// What the sender must aggregate-sign before broadcast. Build the
/// signature with `musig::Signature::sign_multi(keys, items, t)` where
/// `t` is a `b"flamevm.signtx"` transcript with `txid` appended under
/// `b"txid"`; pass the result to [`UnsignedTx::sign`].
pub struct SigningInstructions {
    pub txid: TxID,
    /// The tx-bound `(verification_key, cell_id)` authorizations, in
    /// `signtx` order — the multi-message context to sign.
    pub items: Vec<(CompressedRistretto, crate::cell::CellID)>,
}

/// A built-but-unsigned external transaction (lifecycle step 1→2).
pub struct UnsignedTx {
    header: TxHeader,
    script: Vec<u8>,
    proof: R1CSProof,
    log: TxLog,
    metrics: TxMetrics,
    txbound_items: Vec<(CompressedRistretto, crate::cell::CellID)>,
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
            signature,
            proof: self.proof,
        }
    }
}

impl Program {
    /// Lifecycle step 1: build an unsigned external transaction by
    /// running the witness-bearing program through the prover.
    /// Bulletproof generators are managed inside the crate.
    pub fn build_tx(self, header: TxHeader, limits: Limits) -> Result<UnsignedTx, VMError> {
        let pc_gens = PedersenGens::default();
        let result = Prover::prove(&pc_gens, self, header, limits.gas, limits.mem)?;
        let txbound_items = result
            .deferred_sigs
            .iter()
            .filter_map(|s| match s {
                DeferredSig::TxBound { verification_key, cell_id } => {
                    Some((*verification_key, *cell_id))
                }
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
                vbytes_used: result.vbytes_used,
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
}

/// Read-only handle to the chain's actor state for running one internal
/// transaction. [`Message::execute_tx`] runs against a fresh
/// [`Env::working_copy`]; the env itself is untouched until
/// [`Env::apply_changes`] replays the resulting effects.
pub trait Env {
    /// A fresh mutable working copy of the actor registry for one tx.
    fn working_copy(&self) -> Box<dyn ActorRegistry>;
    /// Current block height (block context for the internal tx).
    fn height(&self) -> u64;
    /// Applies an internal transaction's effects to this state.
    fn apply_changes(&mut self, log: &TxLog);
}

impl Message {
    /// Lifecycle step 4: run this send as an internal transaction
    /// against a read-only [`Env`]. Returns the effects to apply; the
    /// env is untouched until [`Env::apply_changes`]. Internal gas/mem
    /// derive from the Send and the target's size, so `limits` is
    /// currently advisory.
    pub fn execute_tx(self, _limits: Limits, env: &dyn Env) -> Result<InternalTx, VMError> {
        let mut registry = env.working_copy();
        let block = BlockContext { height: env.height() };
        // Internal-tx header: fixed default for now — its source is part
        // of the deferred vbytes/lifecycle design.
        let header = TxHeader { version: 1, locktime: 0 };
        let result = VM::execute_internal(header, self, registry.as_mut(), &block)?;
        Ok(InternalTx {
            log: TxLog(result.txlog),
            metrics: TxMetrics {
                gas_used: result.gas_used,
                total_fee: result.total_fee,
                vbytes_used: result.vbytes_used,
            },
        })
    }
}

/// Reference [`Env`] backed by an in-memory [`MemRegistry`] at a fixed
/// block height. For tests and pre-storage node use.
pub struct MemEnv {
    pub registry: MemRegistry,
    pub height: u64,
}

impl Env for MemEnv {
    fn working_copy(&self) -> Box<dyn ActorRegistry> {
        Box::new(self.registry.clone())
    }
    fn height(&self) -> u64 {
        self.height
    }
    fn apply_changes(&mut self, log: &TxLog) {
        // First cut: replay actor-state saves onto existing actors.
        // Deploy / vbyte-credit / reaping flow is deferred — see
        // design.md §Transaction lifecycle & API (vbytes flow note).
        for entry in log.iter() {
            match entry {
                TxEntry::ActorSave { actor, state } => {
                    if let Some(a) = self.registry.actor_mut(actor) {
                        a.state = Some(state.clone());
                    }
                }
                TxEntry::SetCode { actor, code } => {
                    if let Some(a) = self.registry.actor_mut(actor) {
                        a.code = code.clone();
                    }
                }
                // Exhaustive on purpose: a newly-added effect variant must
                // force a decision here rather than be silently dropped.
                // The following are no-ops for this first-cut applier
                // (deploy/vbyte-credit/reaping deferred — vbytes-flow note).
                TxEntry::Header(_)
                | TxEntry::Data(_)
                | TxEntry::Input(_)
                | TxEntry::Receive(_)
                | TxEntry::Output(_)
                | TxEntry::IssuePub(_, _)
                | TxEntry::IssuePriv(_, _)
                | TxEntry::Retire(_, _)
                | TxEntry::Fee(_)
                | TxEntry::Send(_) => {}
            }
        }
    }
}

/// Transaction ID is a unique 32-byte identifier of a transaction effects represented by `TxLog`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxID(pub Hash);

/// Entry in a transaction log. All entries are hashed into a [transaction ID](TxID).
///
/// `Clone`/`Serialize`/`Deserialize` are still withheld (the linear
/// `Cell`/`Token` payloads don't participate); downstream code wanting
/// those should hash entries to bytes first.
#[derive(Debug)]
pub enum TxEntry {
    /// Tx header — bound at run start as the first txlog entry so
    /// `version` and `locktime` participate in `TxID::from_log`.
    Header(TxHeader),

    /// Plain data entry created by `log` instruction. Contains arbitrary binary string.
    Data(Vec<u8>),

    /// Input: a consumed cell's identity. Emitted by `input` (external-only).
    /// Commits the cell that the transaction has consumed without re-storing
    /// the payload — the existence of the cell is independently asserted by
    /// the Utreexo proof outside the VM.
    Input(crate::cell::CellID),

    /// Receive: the SendID consumed by an internal transaction. Emitted
    /// by `VM::execute_internal` as the first effect after `Header`,
    /// committing the originating `Send`'s anchor into the Internal TxID
    /// merkle root. Without this entry, the Internal TxID would not
    /// commit to its triggering Send (the originating `TxEntry::Send`
    /// lives in the *external* tx's log, hence in External TxID only).
    /// Symmetric with `Input` for external transactions: both are
    /// "what triggered me" effects emitted before the body runs.
    Receive([u8; 32]),

    /// Output: a newly sealed cell, emitted by the `output` opcode.
    Output(crate::Cell),

    /// Cleartext issuance (emitted by `op_issuepub`). Carries the
    /// cleartext `(qty, flv)` pair as `Int253`s — public on the wire,
    /// directly auditable. Flavor is `flavor_from_actor(actor, tag)`
    /// for the actor that ran `issuepub`.
    IssuePub(crate::Int253, crate::Int253),

    /// Confidential issuance (emitted by `op_issuepriv`). Carries
    /// `(qty_point, flv_point)` — the live Pedersen commitment to the
    /// qty and the unblinded commitment to the flavor scalar. Flavor
    /// is `flavor_from_predicate(predicate, tag)` for the predicate
    /// whose `CellOpen` frame ran `issuepriv`. Soundness of the qty
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
    /// actor's identity and the **full** post-save state Dict —
    /// symmetric with `Output(Cell)` which carries the full Cell.
    /// The state machine consumes this entry by replacing the
    /// actor's stored state with `state`; no re-execution of the
    /// script needed. See design.md §"TxLog records effects, not
    /// control flow".
    ///
    /// The MerkleItem encoding hashes `(actor.to_hash(),
    /// state_root(&state))` — i.e. the merkle leaf commits to the
    /// canonical state root, not the full bytes, just as
    /// `Output(Cell)`'s leaf commits to `cell.id()`.
    ActorSave {
        actor: crate::actor::ActorID,
        state: crate::Value,
    },

    /// Actor-code replacement recorded by `setcode`. Carries the full
    /// new code blob for state-machine replay; the merkle leaf commits
    /// to `(actor.to_hash(), code_root(&code))`. Symmetric with
    /// `ActorSave`. See ADR 0018.
    SetCode {
        actor: crate::actor::ActorID,
        code: Vec<u8>,
    },

    /// Outbound asynchronous message scheduled by `op_send`. Carries
    /// the full [`Message`](crate::send::Message) — its `anchor` is
    /// the `left` half of a split of `last_anchor` at the send site,
    /// and its `id()` is the canonical SendID (deterministic at
    /// broadcast time, identifies the future internal-tx delivery).
    ///
    /// The block builder reads `TxEntry::Send` entries directly from
    /// the TxLog — there is no separate "sends" queue — and feeds the
    /// embedded `Message` straight into `VM::execute_internal`.
    /// Symmetric with `TxEntry::Output(Cell)`: each effect that owns
    /// an addressable artifact embeds the artifact itself.
    Send(crate::send::Message),
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
                // matches the wire format (design.md ADR 0006).
                t.append_message(b"tx.version", &h.version.to_le_bytes());
                t.append_message(b"tx.locktime", &h.locktime.to_le_bytes());
            }
            TxEntry::Data(bytes) => {
                t.append_message(b"data", bytes);
            }
            TxEntry::Input(cell_id) => {
                t.append_message(b"input", cell_id);
            }
            TxEntry::Receive(send_id) => {
                t.append_message(b"receive.send_id", send_id);
            }
            TxEntry::Output(cell) => {
                // Bind to the cell's canonical 32-byte identity hash.
                // Cell::id() already absorbs predicate / anchor /
                // payload bytes via Merlin.
                let id = cell.id();
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
                // Little-endian u64, per design.md "Wire format:
                // little-endian everywhere". Domain tag distinguishes
                // this from any other 8-byte append.
                t.append_message(b"fee.qty", &qty.to_le_bytes());
            }
            TxEntry::ActorSave { actor, state } => {
                // Bind every actor-state mutation into the TxID merkle
                // root: actor identity (canonical 32-byte hash, not
                // variant-tagged wire form) + state root. State bytes
                // ride in the entry itself; the merkle leaf commits
                // only to the root, matching Output's Cell-as-id
                // pattern.
                t.append_message(b"save.actor", &actor.to_hash());
                t.append_message(b"save.post_state_root", &crate::actor::state_root(state));
            }
            TxEntry::SetCode { actor, code } => {
                t.append_message(b"setcode.actor", &actor.to_hash());
                t.append_message(b"setcode.code_root", &crate::actor::code_root(code));
            }
            TxEntry::Send(msg) => {
                // Bind to the send's canonical 32-byte SendID hash,
                // analogous to `Output(Cell)` committing only to
                // `cell.id()`. `Message::id()` absorbs the message's
                // canonical wire encoding under domain
                // `flamevm.send.id`, so this single leaf commits to
                // every parameter the future internal tx will be
                // delivered with.
                t.append_message(b"send", msg.id().as_bytes());
            }
        }
    }
}