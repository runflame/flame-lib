//! Tests for `vm.rs`, broken out into feature-area submodules.
//!
//! `vm.rs` declares this module with
//! `#[cfg(test)] #[path = "tests/mod.rs"] mod tests;`, so this is
//! a *child* of `vm` — submodules below access `vm.rs`'s private
//! items via `use super::super::*;`. Shared fixtures live in
//! [`test_helpers`] and are imported via
//! `use super::test_helpers::*;`.

pub(crate) mod mem_registry;
pub(crate) mod test_helpers;

mod test_actor;
mod test_actor_call;
mod test_actor_introspection;
mod test_actor_send;
mod test_actor_state;
mod test_authorization;
mod test_cell_ops;
mod test_cell_transforms;
mod test_commitments;
mod test_confidential_nm;
mod test_confidential_value;
mod test_constraints;
mod test_contexts;
mod test_contracts;
mod test_control_flow;
mod test_dict_ops;
mod test_differential;
mod test_dispatch;
mod test_fee;
mod test_golden;
mod test_hashing;
mod test_integration;
mod test_msm;
mod test_program_cells;
mod test_proof_pipeline;
mod test_scalar_ops;
mod test_stack;
mod test_string_ops;
mod test_tokens;
mod test_tx_introspection;
mod test_txlog;
mod test_witness;
