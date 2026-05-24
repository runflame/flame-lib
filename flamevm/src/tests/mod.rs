//! Tests for `vm.rs`, broken out into feature-area submodules.
//!
//! `vm.rs` declares this module with
//! `#[cfg(test)] #[path = "tests/mod.rs"] mod tests;`, so this is
//! a *child* of `vm` — submodules below access `vm.rs`'s private
//! items via `use super::super::*;`. Shared fixtures live in
//! [`test_helpers`] and are imported via
//! `use super::test_helpers::*;`.

pub(crate) mod test_helpers;

mod test_authorization;
mod test_cells;
mod test_commitments;
mod test_confidential_nm;
mod test_confidential_value;
mod test_constraints;
mod test_control_flow;
mod test_dict_ops;
mod test_dispatch;
mod test_fee;
mod test_hashing;
mod test_int253_ops;
mod test_proof_pipeline;
mod test_stack;
mod test_string_ops;
mod test_tokens;
mod test_txlog;
mod test_actor_state;
mod test_witness;
