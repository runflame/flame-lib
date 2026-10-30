//! The two persistent actors of the DEX, as FlameVM programs.
//!
//! A **coin** actor issues one public flavor, `flavor_from_actor(coin, tag)`,
//! up to a fixed supply and at most `cap` units per request. A **pool**
//! actor holds a reserve of two flavors and trades them at a constant
//! product with a 0.3% fee, issuing its own `LP` flavor to liquidity
//! providers. Everything an actor holds is a `ClearToken`: an actor runs
//! without a constraint system, so it can only count what it can see.
//!
//! Both are deployed by an `Init` address whose constructor buys storage
//! with the Flame it is sent, installs the runtime code and saves the first
//! state. The constructor carries the coin's tag and limits or the pool's
//! two flavors, so the actor id names what the actor is.
//!
//! Every message is one Dict:
//!
//! | key | coin | pool |
//! |---|---|---|
//! | 0 | op | op |
//! | 1 | amount (mint) | token in (swap), token A (add), LP (remove), Flame (renew) |
//! | 2 | — | token B (add) |
//! | 3 | recipient point | recipient point |
//! | 4 | — | minimum out (swap) |
//!
//! `OP_RENEW` on either actor merges an optional Flame token (key 1) into
//! the actor's storage fund and buys another `RENEW_BYTES` lease from it,
//! so anyone can keep an actor from freezing.
//!
//! A swap whose output falls short of its minimum pays the input back to
//! the recipient instead of failing: a failed delivery would seal the whole
//! message, token and all, in a refund contract no wallet knows how to
//! spend. Malformed messages still fail and bounce.
//!
//! Stack comments list the stack bottom to top; `P` is the message, `S`
//! the actor's state.

use flamevm::{CellValue, Scalar, ScriptBuilder};

pub const OP_MINT: u64 = 1;
pub const OP_SWAP: u64 = 1;
pub const OP_ADD: u64 = 2;
pub const OP_REMOVE: u64 = 3;
pub const OP_RENEW: u64 = 4;

pub const KEY_OP: u64 = 0;
pub const KEY_TOKEN: u64 = 1;
pub const KEY_TOKEN_B: u64 = 2;
pub const KEY_RECIPIENT: u64 = 3;
pub const KEY_MIN: u64 = 4;

/// What a constructor buys before it installs the code: room for the code,
/// the state and the lease records.
pub const DEPLOY_BYTES: u64 = 2_048;
/// What one renewal buys.
pub const RENEW_BYTES: u64 = 2_048;

/// State keys.
pub const COIN_SUPPLY: u64 = 0;
pub const COIN_TAG: u64 = 1;
pub const COIN_MAX: u64 = 2;
pub const COIN_CAP: u64 = 3;
pub const POOL_A: u64 = 0;
pub const POOL_B: u64 = 1;
pub const POOL_SUPPLY: u64 = 2;
pub const FUND: u64 = 9;

/// The LP flavor's tag under the pool's actor id.
pub const LP_TAG: &[u8] = b"LP";

/// Of every thousand units paid in, what the price is computed on; the
/// other three stay in the pool for its liquidity providers.
pub const FEE_KEEP: u64 = 997;

/// The offset `ge` compares through. Every quantity it compares is below
/// 2^126: token quantities are u64, and LP supply grows with them.
const OFFSET: u128 = 1 << 126;

trait Ops: Sized {
    fn op(self) -> ScriptBuilder;

    /// `d` → `v d`: removes the value at `k`, leaving the Dict on top.
    /// Hard-fails when absent.
    fn take(self, k: u64) -> ScriptBuilder {
        self.op().push_int(k).get().roll_k(1).drop_().roll_k(1)
    }

    /// `v d` → `d'`: inserts `v` at `k`.
    fn store(self, k: u64) -> ScriptBuilder {
        self.op().push_int(k).roll_k(2).put()
    }

    /// `a b` → `a >= b`, as 1 or 0: 1 exactly when `0 <= a - b < OFFSET`.
    fn ge(self) -> ScriptBuilder {
        self.op()
            .neg()
            .add()
            .push_int(OFFSET)
            .add()
            .push_int(OFFSET)
            .divmod()
            .drop_()
            .push_int(1u64)
            .eqd()
    }

    /// `a b` → `a == b`. Cleartext `eq` peeks and leaves both operands;
    /// this drops them.
    fn eqd(self) -> ScriptBuilder {
        self.op().eq().roll_k(2).drop_().roll_k(1).drop_()
    }

    /// `t` → `t q`: a ClearToken's quantity.
    fn qty(self) -> ScriptBuilder {
        self.op().amount().drop_()
    }

    /// `d` → ø, failing unless the Dict is empty: a message's stray fields,
    /// tokens among them, must not vanish.
    fn drop_empty(self) -> ScriptBuilder {
        self.op().size().push_int(0u64).eqd().verify().drop_()
    }
}

impl Ops for ScriptBuilder {
    fn op(self) -> ScriptBuilder {
        self
    }
}

fn bytes(b: &[u8]) -> CellValue {
    CellValue::from_bytes(b.to_vec()).expect("a short literal fits one cell")
}

/// A program as its native Cell graph: bytecode alone would drop the
/// literal Cells it pushes.
fn code(script: ScriptBuilder) -> CellValue {
    CellValue::from(script.to_cell().expect("actor code encodes"))
}

/// The constructor every actor shares: `P{1: Flame}` → buys `DEPLOY_BYTES`,
/// keeps the rest of the Flame as the storage fund, saves what `init`
/// makes of `S{9: fund}` and installs `runtime`.
fn constructor(
    runtime: ScriptBuilder,
    init: impl FnOnce(ScriptBuilder) -> ScriptBuilder,
) -> ScriptBuilder {
    let s = ScriptBuilder::new()
        // P
        .take(KEY_TOKEN)
        .drop_empty()
        // flame
        .push_int(DEPLOY_BYTES)
        .addstorage()
        .verify()
        .merge()
        .verify()
        // fund
        .push_int(0u64)
        .dict()
        .store(FUND);
    // S
    init(s)
        .push_literal(code(runtime))
        .setcode()
        .load()
        .drop_()
        .save()
}

/// `P op` → ø: merges token 1, if any, into the fund and buys another
/// lease out of it. Both runtimes share it.
fn renew(s: ScriptBuilder) -> ScriptBuilder {
    s.drop_()
        .load()
        // P S
        .take(FUND)
        .roll_k(2)
        // fund S P
        .push_int(KEY_TOKEN)
        .get_opt()
        // fund S P {t 1 | 0}
        .build_if(|s| {
            // fund S P t
            s.roll_k(3)
                .merge()
                .verify()
                // S P fund'
                .roll_k(2)
                .roll_k(2)
        })
        // fund S P
        .drop_empty()
        .roll_k(1)
        .push_int(RENEW_BYTES)
        .addstorage()
        .verify()
        .merge()
        .verify()
        // S fund'
        .roll_k(1)
        .store(FUND)
        .save()
}

/// `P` → `P op is_renew`
fn dispatch(s: ScriptBuilder) -> ScriptBuilder {
    s.take(KEY_OP).roll_k(1).dup_k(0).push_int(OP_RENEW).eqd()
}

/// The coin's runtime: mint and renew.
pub fn coin_runtime(tag: &[u8]) -> ScriptBuilder {
    let tag = tag.to_vec();
    dispatch(ScriptBuilder::new()).build_if_else(renew, move |s| {
        s.push_int(OP_MINT)
            .eqd()
            .verify()
            .take(KEY_TOKEN)
            .take(KEY_RECIPIENT)
            .drop_empty()
            // amt rcpt
            .load()
            .take(COIN_SUPPLY)
            .roll_k(1)
            // amt rcpt S supply
            .dup_k(3)
            .add()
            .roll_k(1)
            // amt rcpt supply' S
            .push_int(COIN_MAX)
            .get_dup()
            .verify()
            .dup_k(2)
            .ge()
            .verify()
            // max >= supply'
            .push_int(COIN_CAP)
            .get_dup()
            .verify()
            .dup_k(4)
            .ge()
            .verify()
            // cap >= amt
            .dup_k(3)
            .push_int(1u64)
            .ge()
            .verify()
            // amt >= 1, which also keeps a negative amount out
            .store(COIN_SUPPLY)
            .save()
            // amt rcpt
            .roll_k(1)
            .push_literal(bytes(&tag))
            .issuepub()
            .roll_k(1)
            .output()
    })
}

/// The coin's constructor: supply 0 of `max`, at most `cap` per mint.
pub fn coin_constructor(tag: &[u8], max: u64, cap: u64) -> ScriptBuilder {
    let tag = tag.to_vec();
    constructor(coin_runtime(&tag), move |s| {
        s.push_int(0u64)
            .roll_k(1)
            .store(COIN_SUPPLY)
            .push_literal(bytes(&tag))
            .roll_k(1)
            .store(COIN_TAG)
            .push_int(max)
            .roll_k(1)
            .store(COIN_MAX)
            .push_int(cap)
            .roll_k(1)
            .store(COIN_CAP)
    })
}

/// The pool's runtime over flavors `a` and `b`; only `a` is needed to tell
/// the direction of a swap.
pub fn pool_runtime(a: Scalar) -> ScriptBuilder {
    dispatch(ScriptBuilder::new()).build_if_else(renew, move |s| {
        s.dup_k(0).push_int(OP_SWAP).eqd().build_if_else(
            move |s| swap(s, a),
            |s| {
                s.dup_k(0)
                    .push_int(OP_ADD)
                    .eqd()
                    .build_if_else(add_liquidity, |s| {
                        s.dup_k(0)
                            .push_int(OP_REMOVE)
                            .eqd()
                            .verify()
                            .pipe(remove_liquidity)
                    })
            },
        )
    })
}

trait Pipe: Sized {
    fn pipe(self, f: impl FnOnce(Self) -> Self) -> Self {
        f(self)
    }
}

impl Pipe for ScriptBuilder {}

/// `P op` → ø. Trades token 1 for the other flavor at the constant product,
/// paying `rcpt` at least `min`, or paying the input back.
fn swap(s: ScriptBuilder, a: Scalar) -> ScriptBuilder {
    s.drop_()
        .take(KEY_TOKEN)
        .take(KEY_RECIPIENT)
        .take(KEY_MIN)
        .drop_empty()
        // tin rcpt min
        .roll_k(2)
        .load()
        .take(POOL_A)
        .take(POOL_B)
        // rcpt min tin RA RB S
        .roll_k(3)
        .amount()
        .push_int(a)
        .eqd()
        // rcpt min RA RB S tin q isA
        .dup_k(0)
        .not()
        .build_if(|s| {
            s.roll_k(4)
                .roll_k(5)
                .roll_k(5)
                .roll_k(5)
                .roll_k(5)
                .roll_k(5)
        })
        // rcpt min X Y S tin q isA, X of tin's flavor
        .roll_k(3)
        .roll_k(5)
        .roll_k(5)
        .roll_k(5)
        .roll_k(5)
        // rcpt min isA S X Y tin q
        .roll_k(3)
        .qty()
        .roll_k(4)
        .qty()
        // rcpt min isA S tin q X qx Y qy
        .dup_k(4)
        .push_int(FEE_KEEP)
        .mul()
        .dup_k(0)
        .dup_k(2)
        .mul()
        // ... qy q997 num
        .dup_k(4)
        .push_int(1_000u64)
        .mul()
        .roll_k(2)
        .add()
        // ... qy num den
        .divmod()
        .drop_()
        // rcpt min isA S tin q X qx Y qy out
        .dup_k(0)
        .dup_k(10)
        .ge()
        .build_if_else(
            |s| {
                s.roll_k(1)
                    .drop_()
                    .split()
                    // rcpt min isA S tin q X qx Y' tout
                    .dup_k(9)
                    .output()
                    .roll_k(1)
                    .drop_()
                    // rcpt min isA S tin q X Y'
                    .roll_k(3)
                    .roll_k(2)
                    .merge()
                    .verify()
                    // rcpt min isA S q Y' X'
                    .roll_k(2)
                    .drop_()
                    .roll_k(1)
            },
            |s| {
                s.drop_()
                    .drop_()
                    .roll_k(1)
                    .drop_()
                    // rcpt min isA S tin q X Y
                    .roll_k(3)
                    .dup_k(7)
                    .output()
                    .roll_k(2)
                    .drop_()
            },
        )
        // rcpt min isA S X Y: Y goes back under key isA, X under 1 - isA
        .dup_k(3)
        .roll_k(3)
        .roll_k(2)
        .roll_k(2)
        .roll_k(1)
        .put()
        // rcpt min isA X S
        .dup_k(2)
        .push_int(1u64)
        .roll_k(1)
        .neg()
        .add()
        .roll_k(2)
        .put()
        .save()
        .drop_()
        .drop_()
        .drop_()
}

/// `P op` → ø. Deposits token 1 (flavor A) and token 2 (flavor B) and pays
/// `rcpt` LP tokens: the first deposit mints one per unit of A, later ones
/// the smaller of their two proportional shares. What a deposit brings
/// beyond that share stays in the pool.
fn add_liquidity(s: ScriptBuilder) -> ScriptBuilder {
    s.drop_()
        .take(KEY_TOKEN)
        .take(KEY_TOKEN_B)
        .take(KEY_RECIPIENT)
        .drop_empty()
        .load()
        .take(POOL_SUPPLY)
        .take(POOL_A)
        .take(POOL_B)
        // ta tb rcpt L RA RB S
        .roll_k(6)
        .roll_k(6)
        // rcpt L RA RB S ta tb
        .roll_k(2)
        .roll_k(5)
        .roll_k(5)
        .roll_k(5)
        .roll_k(5)
        .roll_k(5)
        // rcpt S L RA RB ta tb
        .qty()
        .roll_k(2)
        .qty()
        // rcpt S L RA RB tb qb ta qa
        .roll_k(4)
        .qty()
        .roll_k(6)
        .qty()
        // rcpt S L tb qb ta qa RB qRB RA qRA
        .dup_k(8)
        .push_int(0u64)
        .eqd()
        .build_if_else(
            |s| s.dup_k(4),
            |s| {
                s.dup_k(4)
                    .dup_k(9)
                    .mul()
                    .dup_k(1)
                    .divmod()
                    .drop_()
                    // ... qRA sA
                    .dup_k(7)
                    .dup_k(10)
                    .mul()
                    .dup_k(4)
                    .divmod()
                    .drop_()
                    // ... qRA sA sB
                    .dup_k(1)
                    .dup_k(1)
                    .ge()
                    .build_if_else(|s| s.roll_k(1).drop_(), |s| s.drop_())
            },
        )
        // rcpt S L tb qb ta qa RB qRB RA qRA shares
        .roll_k(1)
        .drop_()
        .roll_k(2)
        .drop_()
        .roll_k(3)
        .drop_()
        .roll_k(4)
        .drop_()
        // rcpt S L tb ta RB RA shares
        .dup_k(0)
        .push_int(1u64)
        .ge()
        .verify()
        .roll_k(3)
        .roll_k(2)
        .merge()
        .verify()
        // rcpt S L tb RB shares RA'
        .roll_k(3)
        .roll_k(3)
        .merge()
        .verify()
        // rcpt S L shares RA' RB'
        .roll_k(4)
        .store(POOL_B)
        .store(POOL_A)
        // rcpt L shares S
        .roll_k(2)
        .dup_k(2)
        .add()
        .roll_k(1)
        .store(POOL_SUPPLY)
        .save()
        // rcpt shares
        .push_literal(bytes(LP_TAG))
        .issuepub()
        .roll_k(1)
        .output()
}

/// `P op` → ø. Retires LP token 1 and pays `rcpt` its share of each
/// reserve.
fn remove_liquidity(s: ScriptBuilder) -> ScriptBuilder {
    s.drop_()
        .take(KEY_TOKEN)
        .take(KEY_RECIPIENT)
        .drop_empty()
        // lp rcpt
        .roll_k(1)
        .amount()
        .selfid()
        .push_literal(bytes(LP_TAG))
        .issuepubflv()
        .eqd()
        .verify()
        // rcpt lp q
        .roll_k(1)
        .retire()
        .load()
        .take(POOL_SUPPLY)
        .roll_k(1)
        // rcpt q S L
        .dup_k(0)
        .dup_k(3)
        .ge()
        .verify()
        .roll_k(1)
        // rcpt q L S
        .pipe(|s| pay_share(s, POOL_A))
        .pipe(|s| pay_share(s, POOL_B))
        .roll_k(1)
        .roll_k(2)
        .neg()
        .add()
        // rcpt S L'
        .roll_k(1)
        .store(POOL_SUPPLY)
        .save()
        .drop_()
}

/// `rcpt q L S` → `rcpt q L S'`: pays `rcpt` the share `q / L` of the
/// reserve at `key`.
fn pay_share(s: ScriptBuilder, key: u64) -> ScriptBuilder {
    s.take(key)
        .roll_k(1)
        .qty()
        // rcpt q L S R qR
        .dup_k(4)
        .mul()
        .dup_k(3)
        .divmod()
        .drop_()
        .split()
        // rcpt q L S R' out
        .dup_k(5)
        .output()
        .roll_k(1)
        .store(key)
}

/// The pool's constructor: empty reserves of `a` and `b`, no LP tokens.
pub fn pool_constructor(a: Scalar, b: Scalar) -> ScriptBuilder {
    constructor(pool_runtime(a), move |s| {
        s.push_int(0u64)
            .push_int(a)
            .borrow()
            .roll_k(1)
            .drop_()
            .roll_k(1)
            .store(POOL_A)
            .push_int(0u64)
            .push_int(b)
            .borrow()
            .roll_k(1)
            .drop_()
            .roll_k(1)
            .store(POOL_B)
            .push_int(0u64)
            .roll_k(1)
            .store(POOL_SUPPLY)
    })
}
