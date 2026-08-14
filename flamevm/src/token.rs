//! Token, ClearToken, WideToken — linear asset value types.

use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;

use crate::constraints::Commitment;
use crate::actor::ActorID;
use crate::{Int253, Predicate, String};

/// Canonical flavor of the native Flame token.
pub const FLAME_FLAVOR: Int253 = Int253::ZERO;

// ── Token ────────────────────────────────────────────────────────

/// Encrypted asset value — Pedersen commitments to a non-negative
/// quantity and a flavor scalar. Portable across cells and actor state.
///
/// The two halves are independent [`Commitment`]s, each of which may
/// be [`Commitment::Open`] (carrying the prover's witness — value and
/// blinding factor) or [`Commitment::Closed`] (only the compressed
/// Ristretto point). The encrypted [`Token`] is a **bearer** value:
/// once on the stack, it can only leave through `output` (sealed into
/// a cell), `retire`, or `mix`/`decrypt`.
///
/// Non-copyable, non-droppable. The qty is range-proven non-negative
/// at construction time by the CS opcodes that produce a Token (the
/// encrypted `borrow` / `mix` / `issue` branches). The cleartext
/// convenience constructor below trivially satisfies the range
/// invariant for cleartext-only call sites.
#[derive(Clone, Debug)]
pub struct Token {
    /// Pedersen commitment to the asset's quantity (≥ 0).
    pub qty: Commitment,
    /// Pedersen commitment to the asset's flavor scalar.
    pub flv: Commitment,
}

impl Token {
    /// Builds a `Token` from already-constructed commitments. Used
    /// by the encrypted opcode branches (`borrow`, `mix`).
    pub fn new(qty: Commitment, flv: Commitment) -> Self {
        Token { qty, flv }
    }

    /// Builds a `Token` from cleartext `Int253`s by wrapping each in an
    /// **unblinded** open commitment. Useful for tests, for the
    /// cleartext branch of `issue`, and as a stable on-stack
    /// representation that scripts can carry forward through the
    /// confidential opcodes without surfacing the cleartext.
    ///
    /// Caller responsibility: `qty` should be non-negative. Opcodes
    /// that create Tokens via this path (only `issue`'s cleartext
    /// branch and tests) enforce non-negativity at the call site;
    /// the constructor itself does not.
    pub fn cleartext(qty: Int253, flv: Int253) -> Self {
        Token {
            qty: Commitment::unblinded(qty),
            flv: Commitment::unblinded(flv),
        }
    }
}

// ── WideToken ────────────────────────────────────────────────────

/// Stack-only, possibly-negative encrypted value. Wraps a
/// [`spacesuit::AllocatedValue`] — a pair of R1CS variables (quantity,
/// flavor) and an optional cleartext assignment.
///
/// `WideToken` is the intermediate type emitted by `borrow` (negative
/// half), `fee`, and the intermediate steps of `mix`/`cloak`. It is
/// **non-portable** (cannot be sealed into a cell) because its
/// quantity is not range-proven. The CS-touching opcodes that
/// produce or consume it are `borrow` (encrypted branch),
/// `mix`/`cloak`, and `fee`.
///
/// The wrapper is `pub(crate)` over the inner `AllocatedValue` to keep
/// the spacesuit dependency from leaking into the public API; the
/// CS-bound opcode handlers construct it directly.
#[derive(Copy, Clone, Debug)]
pub struct WideToken(pub(crate) spacesuit::AllocatedValue);

impl WideToken {
    /// Accessor for the underlying R1CS variable bundle. Used by
    /// the CS-touching opcodes and not exposed beyond the crate.
    pub(crate) fn allocated(&self) -> &spacesuit::AllocatedValue {
        &self.0
    }
}

// ── ClearToken ───────────────────────────────────────────────────

/// Cleartext asset value: quantity and flavor are both `Int253`.
///
/// **Portability rules** (enforced by `Value::is_portable`):
/// - `qty ≥ 0` → portable: a non-negative cleartext token can be
///   sealed into a cell or actor state.
/// - `qty < 0` → non-portable: a negative cleartext token is a debt
///   token and cannot be sealed.
///
/// **Droppability rules** (enforced by `Value::is_droppable`):
/// - `qty == 0` → droppable (zero-qty bearer of a flavor; no value).
/// - `qty != 0` → not droppable (would silently destroy value).
///
/// Non-copyable in all cases (linear-type discipline).
#[derive(Copy, Clone, Debug)]
pub struct ClearToken {
    pub(crate) qty: Int253,
    pub(crate) flv: Int253,
}

impl ClearToken {
    /// Constructs a cleartoken with the given quantity and flavor.
    /// No sign or range check at construction — portability/dropability
    /// are determined dynamically at each opcode that inspects them.
    pub fn new(qty: Int253, flv: Int253) -> Self {
        ClearToken { qty, flv }
    }

    /// Read-only access to the cleartext quantity.
    pub fn qty(&self) -> Int253 {
        self.qty
    }

    /// Read-only access to the cleartext flavor.
    pub fn flv(&self) -> Int253 {
        self.flv
    }

    /// `true` iff `qty == 0` (drop-eligible per spec.md `drop`).
    pub fn is_zero_qty(&self) -> bool {
        self.qty.is_zero()
    }

    /// Combines two `ClearToken`s with the same flavor by summing
    /// their quantities. Returns the merged token, or — on flavor
    /// mismatch — the two original tokens unchanged (for the soft-fail
    /// path used by the `merge` opcode).
    #[allow(clippy::result_large_err)]
    pub fn merge_into(self, other: ClearToken) -> Result<ClearToken, (ClearToken, ClearToken)> {
        if self.flv != other.flv {
            return Err((self, other));
        }
        Ok(ClearToken {
            qty: self.qty + other.qty,
            flv: self.flv,
        })
    }

    /// Splits `self` into two `ClearToken`s of the same flavor: a
    /// remainder with `qty = self.qty - q` and a new token with
    /// `qty = q`. Returns `None` if `q > self.qty` or if any operand is
    /// negative — `split` is a hard-fail when out of range (per the
    /// Phase-8 plan's `TokenSplitOutOfRange`).
    ///
    /// `q` itself must be non-negative; a negative `q` is rejected.
    pub fn split(self, q: Int253) -> Option<(ClearToken, ClearToken)> {
        if q.is_negative() || self.qty.is_negative() {
            return None;
        }
        if q > self.qty {
            return None;
        }
        let remainder = ClearToken {
            qty: self.qty - q,
            flv: self.flv,
        };
        let new_token = ClearToken {
            qty: q,
            flv: self.flv,
        };
        Some((remainder, new_token))
    }

    /// Returns a `ClearToken` with negated quantity. Used by the
    /// cleartext branch of the `borrow` opcode to produce the
    /// debit/credit pair.
    pub fn negated(&self) -> ClearToken {
        ClearToken {
            qty: -self.qty,
            flv: self.flv,
        }
    }
}

// ── Flavor helper ────────────────────────────────────────────────

/// Flavor scalar identifying an asset type minted by `actor`. The `tag`
/// lets one actor mint multiple distinct flavors. Always non-negative
/// (wide-mod-order scalar reduction). The Merlin domain is consensus-fixed —
/// a rename is a hard fork.
pub fn flavor_from_actor(actor: &ActorID, tag: &String) -> Int253 {
    let mut t = Transcript::new(b"flamevm.issuepub.flavor");
    // `to_hash()` collapses both enum variants to the canonical
    // 32-byte form; for the Hash variant it's the stored id, for
    // the Constructor variant it's the deterministic seed (per
    // `ActorID::to_hash` docstring).
    t.append_message(b"actor", &actor.to_hash());
    t.append_message(b"tag", &tag.to_bytes_vec());
    let mut buf = [0u8; 64];
    t.challenge_bytes(b"flavor", &mut buf);
    Int253::from(Scalar::from_bytes_mod_order_wide(&buf))
}

/// Flavor scalar identifying an asset type minted under `predicate`. The
/// `tag` lets one predicate mint multiple flavors. Uses the same transcript
/// label as `flavor_from_actor` but a distinct first message (`b"predicate"`
/// vs `b"actor"`), so actor- and predicate-issued flavors can never collide
/// even when their 32-byte identities are numerically equal.
pub fn flavor_from_predicate(
    predicate: &Predicate,
    tag: &String,
) -> Int253 {
    let mut t = Transcript::new(b"flamevm.issuepriv.flavor");
    let point_bytes = predicate.to_point().to_bytes();
    t.append_message(b"predicate", &point_bytes);
    t.append_message(b"tag", &tag.to_bytes_vec());
    let mut buf = [0u8; 64];
    t.challenge_bytes(b"flavor", &mut buf);
    Int253::from(Scalar::from_bytes_mod_order_wide(&buf))
}
