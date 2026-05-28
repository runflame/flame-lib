use bulletproofs::r1cs::R1CSProof;
use curve25519_dalek::ristretto::CompressedRistretto;
use merkle::{Hash, MerkleItem, MerkleTree};
use merlin::Transcript;
use musig::Signature;
use serde::{Deserialize, Serialize};

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

pub struct InternalTx {}

/// Transaction ID is a unique 32-byte identifier of a transaction effects represented by `TxLog`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxID(pub Hash);

/// Entry in a transaction log. All entries are hashed into a [transaction ID](TxID).
///
/// Linear-value variants (`Output` carries a `Cell`) prevent us from
/// deriving `Clone`/`Debug`/`Serialize`/`Deserialize` here; downstream
/// code wanting those should hash entries to bytes first or wrap.
pub enum TxEntry {
    /// Tx header — bound at run start as the first txlog entry so
    /// `version` and `locktime` participate in `TxID::from_log`.
    /// Mirrors zkvm's `TxEntry::Header(TxHeader)`.
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
        state: crate::Dict,
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
    /// Computes the canonical 32-byte transaction identifier as a
    /// merkle root over the txlog entries (header + effect list).
    /// Domain-separated by `flamevm.txid`. Mirrors zkvm's
    /// `TxID::from_log` exactly in shape.
    pub fn from_log(txlog: &[TxEntry]) -> Self {
        TxID(MerkleTree::root(b"flamevm.txid", txlog))
    }
}

/// Manual `Debug` impl — `TxEntry` cannot `#[derive(Debug)]` because
/// the `Output(Cell)` variant carries a linear `Cell`. Prints just the
/// variant tag (and, where cheap, an identifier) so `Result::unwrap_err`
/// and friends compile against `Result<…, VMError>` returns that carry
/// `Vec<TxEntry>` in their `Ok` arm.
impl core::fmt::Debug for TxEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TxEntry::Header(h) => f
                .debug_struct("TxEntry::Header")
                .field("version", &h.version)
                .field("locktime", &h.locktime)
                .finish(),
            TxEntry::Data(bytes) => f
                .debug_struct("TxEntry::Data")
                .field("len", &bytes.len())
                .finish(),
            TxEntry::Input(id) => {
                f.debug_tuple("TxEntry::Input").field(id).finish()
            }
            TxEntry::Receive(send_id) => {
                f.debug_tuple("TxEntry::Receive").field(send_id).finish()
            }
            TxEntry::Output(_) => f.write_str("TxEntry::Output(<cell>)"),
            TxEntry::IssuePub(_, _) => f.write_str("TxEntry::IssuePub(<qty>, <flv>)"),
            TxEntry::IssuePriv(_, _) => f.write_str("TxEntry::IssuePriv(<qty>, <flv>)"),
            TxEntry::Retire(_, _) => {
                f.write_str("TxEntry::Retire(<qty>, <flv>)")
            }
            TxEntry::Fee(q) => {
                f.debug_tuple("TxEntry::Fee").field(q).finish()
            }
            TxEntry::ActorSave { actor, .. } => f
                .debug_struct("TxEntry::ActorSave")
                .field("actor", actor)
                .finish(),
            TxEntry::Send(msg) => {
                f.debug_tuple("TxEntry::Send").field(msg).finish()
            }
        }
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