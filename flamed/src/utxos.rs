//! Every unspent contract, with a proof that is valid right now.
//!
//! This is the Utreexo bridge, and the reason it exists is a single upstream
//! rule: a `Catchup` repairs a proof across exactly one block, so a proof two
//! blocks old is beyond repair. A wallet cannot hold its own proofs unless it
//! follows every block; the node does follow every block, so it refreshes
//! every live proof on every connect and hands out a fresh one on request.

use std::collections::BTreeMap;

use flamechain::utreexo::{Catchup, Proof, UtreexoError};
use flamechain::{utreexo_hasher, ContractLeaf};
use flamevm::ContractID;

/// The unspent set.
#[derive(Debug, Default)]
pub struct UtxoSet {
    unspent: BTreeMap<ContractID, Proof>,
}

impl UtxoSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a contract this block created. It has no merkle path until
    /// the block is normalized, which `apply_catchup` is what does.
    pub fn insert_transient(&mut self, id: ContractID) {
        self.unspent.insert(id, Proof::Transient);
    }

    /// Forgets a contract that has been spent.
    pub fn remove(&mut self, id: &ContractID) -> Option<Proof> {
        self.unspent.remove(id)
    }

    /// The proof this node holds for a contract.
    pub fn get(&self, id: &ContractID) -> Option<&Proof> {
        self.unspent.get(id)
    }

    /// How many contracts are unspent.
    pub fn len(&self) -> usize {
        self.unspent.len()
    }

    /// Whether anything is unspent.
    pub fn is_empty(&self) -> bool {
        self.unspent.is_empty()
    }

    /// Lifts every proof across one block, and turns this block's new ids
    /// from `Transient` into `Committed`.
    ///
    /// In place, through `mem::replace`: `update_proof` takes its proof by
    /// value while the iteration hands out `&mut`. A failure leaves the set
    /// half-lifted and is not recoverable — every caller treats it as
    /// fatal, because a node whose proofs do not match its own chain has
    /// nothing left to serve.
    ///
    /// The `Committed` check is the load-bearing part. `update_proof` does
    /// not fail a transient item that the block did not commit; it hands
    /// back `Transient` again. Serving that as `unspent` would give a
    /// wallet a proof of nothing, and it would find out only when its
    /// spend was refused. Every id in this set was inserted by a block or
    /// by genesis, so the accumulator must have committed it, and if it
    /// did not, this node is wrong about which contracts exist.
    pub fn apply_catchup(&mut self, catchup: &Catchup) -> Result<(), UtreexoError> {
        let hasher = utreexo_hasher::<ContractLeaf>();
        for (id, proof) in self.unspent.iter_mut() {
            let stale = std::mem::replace(proof, Proof::Transient);
            *proof = catchup.update_proof(&ContractLeaf(*id), stale, &hasher)?;
            if proof.as_path().is_none() {
                return Err(UtreexoError::InvalidProof);
            }
        }
        Ok(())
    }
}
