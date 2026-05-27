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

    /// Output: a newly sealed cell, emitted by the `output` opcode.
    Output(crate::Cell),

    /// Issuance: an asset value has just been *created* into circulation.
    /// Carries `(qty_point, flv_point)` — Pedersen commitments to the
    /// quantity and flavor scalars.
    ///
    /// For the cleartext issuance branch (`issue` with `Int253` qty),
    /// both commitments are unblinded (blinding factor = 0). For the
    /// encrypted branch, the commitments are the live
    /// blinded points whose openings are proven through the constraint
    /// system.
    Issue(CompressedRistretto, CompressedRistretto),

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

    /// Actor-state mutation recorded by `op_save`. Carries the actor's
    /// identity and the canonical hash of its post-save state. Together
    /// with the actor's pre-tx state (known from the registry) this
    /// fully defines the effect a thin state-machine applies — no
    /// re-execution of the script needed. See design.md §"TxLog records
    /// effects, not control flow".
    ActorSave {
        actor: crate::actor::ActorID,
        post_state_root: [u8; 32],
    },

    /// Outbound asynchronous message scheduled by `op_send`. The
    /// `anchor` is the `left` half of a split of `last_anchor` at
    /// the send site; it doubles as the SendID (deterministic at
    /// broadcast time, identifies the future internal-tx delivery).
    ///
    /// `payload` is the full argument vector the future internal
    /// tx will receive. The block builder reads `TxEntry::Send`
    /// entries directly from the TxLog — there is no separate
    /// "sends" queue. `caller` is the actor that issued the send
    /// (or `None` if the send originated at ExternalRoot).
    Send {
        anchor: crate::vm::Anchor,
        target: crate::actor::ActorID,
        caller: Option<crate::actor::ActorID>,
        method: crate::Int253,
        refund_predicate: crate::cell::Predicate,
        gas: u64,
        vbytes: u64,
        payload: Vec<crate::Value>,
    },
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
            TxEntry::Output(_) => f.write_str("TxEntry::Output(<cell>)"),
            TxEntry::Issue(_, _) => f.write_str("TxEntry::Issue(<qty>, <flv>)"),
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
            TxEntry::Send {
                target, method, anchor, gas, vbytes, ..
            } => f
                .debug_struct("TxEntry::Send")
                .field("anchor", anchor)
                .field("target", target)
                .field("method", method)
                .field("gas", gas)
                .field("vbytes", vbytes)
                .finish(),
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
            TxEntry::Output(cell) => {
                // Bind to the cell's canonical 32-byte identity hash.
                // Cell::id() already absorbs predicate / anchor /
                // payload bytes via Merlin.
                let id = cell.id();
                t.append_message(b"output", &id);
            }
            TxEntry::Issue(qty_pt, flv_pt) => {
                t.append_message(b"issue.qty", qty_pt.as_bytes());
                t.append_message(b"issue.flv", flv_pt.as_bytes());
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
            TxEntry::ActorSave {
                actor,
                post_state_root,
            } => {
                // Bind every actor-state mutation into the TxID merkle
                // root: actor identity + canonical hash of the new
                // state. The state machine applies these in order to
                // mutate the registry without re-running the script.
                t.append_message(b"save.actor", &actor.to_bytes());
                t.append_message(b"save.post_state_root", post_state_root);
            }
            TxEntry::Send {
                anchor,
                target,
                caller,
                method,
                refund_predicate,
                gas,
                vbytes,
                payload,
            } => {
                // Bind a complete summary of the send into the
                // external TxID merkle root so the (External
                // TxID, SendID) pair commits to every parameter
                // the future internal tx will be delivered with.
                t.append_message(b"send.anchor", &anchor.0);
                t.append_message(b"send.target", &target.to_bytes());
                // Caller absence encoded as 0x00; presence as
                // 0x01 || actor-bytes — fixed-shape encoding to
                // keep the merkle leaf canonical.
                match caller {
                    None => t.append_message(b"send.caller", &[0u8]),
                    Some(c) => {
                        let mut buf = vec![1u8];
                        buf.extend_from_slice(&c.to_bytes());
                        t.append_message(b"send.caller", &buf);
                    }
                }
                t.append_message(b"send.method", &method.to_bytes());
                t.append_message(
                    b"send.refund_predicate",
                    refund_predicate.to_point().as_bytes(),
                );
                t.append_message(b"send.gas", &gas.to_le_bytes());
                t.append_message(b"send.vbytes", &vbytes.to_le_bytes());
                // Iterate the payload values; each value's canonical
                // wire form contributes to the merkle leaf.
                t.append_message(b"send.payload.len", &(payload.len() as u64).to_le_bytes());
                let mut buf = Vec::new();
                for v in payload {
                    buf.clear();
                    crate::encoding::write_value(&mut buf, v)
                        .expect("portable values encode (op_send enforces portability)");
                    t.append_message(b"send.payload.item", &buf);
                }
            }
        }
    }
}