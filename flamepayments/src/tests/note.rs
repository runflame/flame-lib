//! Notes: the two vectors of `payments.md`, its compliance checks, one test
//! per `NoteError`, and a wallet restored from its seed.
//!
//! The hex below is copied from `payments.md` "Test vectors" and nowhere
//! else; the notes are split at the same 32-byte boundaries as there.

use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::utreexo::Proof;
use flamekd::{util, Network, ReceivingAddress};
use flamevm::{
    Anchor, ClearToken, Commitment, Contract, Predicate, Scalar, Token, TxHeader, TxLog, Value,
    FLAME_FLAVOR,
};
use rand::rngs::StdRng;

use super::{header, limits, native_output, output_to, outputs, publish, receive, rng};
use crate::builder::{build_transfer, sign, BuilderError, InputSpec, OutputSpec};
use crate::keys::SpendAccount;
use crate::note::{
    open_note, outputs_with_notes, seal, seal_plaintext, NoteError, MEMO_MAX, NOTE_VERSION,
};

const SEED: [u8; 64] = [0x5e; 64];

/// A distinctive amount for the allocation every transfer here spends.
const ALLOCATION: u64 = 23_456_789_012;

/// One vector of `payments.md`, as the doc prints it.
struct Vector {
    branch: u32,
    address: &'static str,
    v: &'static str,
    r: &'static str,
    qty: u64,
    flv: &'static str,
    memo: &'static [u8],
    s: &'static str,
    big_v: &'static str,
    big_r: &'static str,
    x: &'static str,
    qty_blinding: &'static str,
    flv_blinding: &'static str,
    siv_key: &'static str,
    c_qty: &'static str,
    c_flv: &'static str,
    tag: &'static str,
    note: &'static str,
}

/// Payment with a memo: `m/35263'/1'/0'/0/0`.
const PAYMENT: Vector = Vector {
    branch: util::RECEIVING,
    address: "tf1psxy35zgz289wuj33pm05vkl3sr40xp3ecdacw0dmuzu688wufwaf55kjxcgtw9v6tf5mvta3nk93nj8knx8phhrezer3n8999crggcz0p20m",
    v: "7b4ac2f41156ceb0d35a9631311fe799b2f5c258e36d4f7da7b36d0599ecf804",
    r: "0101010101010101010101010101010101010101010101010101010101010101",
    qty: 40000000000,
    flv: "0100000000000000000000000000000000000000000000000000000000000000",
    memo: b"invoice 42",
    s: "0c0c48d048128e5772518876fa32df8c07579831ce1bdc39eddf05cd1ceee25d",
    big_v: "d4d29691b085b8acd2d34db17d8cec58ce47b4cc70dee3c8b238cce529703423",
    big_r: "3e440469a098036d89ffb2d77a4542928f2f74c2b5769da7480736ace829dc10",
    x: "fa3c8a0c613bb5c159d74ca95045846a59c443fa5897c3b4645a0ecb2980a836",
    qty_blinding: "a4c09913a10fc47b6b6a3ba823b078e20808900d822758d31dfe8f9c3a39df0b",
    flv_blinding: "414e5f225386ac298f99e42f858b307dd0b569ba0b2139f50975209d05712b0f",
    siv_key: "fcbfb5bcb360103d22a8e7601bc81948f4621f170cbd9f433fdc15fecf6c3ff7",
    c_qty: "8c5d58713758003e3a1bec088486046f59bf3dad7219a1633469d0bf9bbb6700",
    c_flv: "38185a383a8984641e76d7490e98528beeddd4c25348f1d95e96353c25a9e51f",
    tag: "d47cb53a92befd875389aa7600883e5c",
    note: concat!(
        "013e440469a098036d89ffb2d77a4542928f2f74c2b5769da7480736ace829dc",
        "10d47cb53a92befd875389aa7600883e5ceaa9693d564bfa2749451ac4a5acdc",
        "1c8d458ab3cecdd2e620cc816089e8a22673e615ca6688bec17efcfa853bd789",
        "d496c2",
    ),
};

/// Change, no memo: `m/35263'/1'/0'/1/0`.
const CHANGE: Vector = Vector {
    branch: util::CHANGE,
    address: "tf1lrkq0dfkh4q83pxp96rp62a4tlnn3ajw9gafvhd2uyg5vv3kmslegyut7k6ymyeygdpr5l4632djpd4a7vlfmt6pcrly3wj2rt2pxcgdelf8q",
    v: "f71608bf3cb3a0dd14548b346ff0895205c884b77cc2759f530a3c08f06af005",
    r: "0202020202020202020202020202020202020202020202020202020202020200",
    qty: 99000000000,
    flv: "0100000000000000000000000000000000000000000000000000000000000000",
    memo: b"",
    s: "f8ec07b536bd407884c12e861d2bb55fe738f64e2a3a965daae111463236dc3f",
    big_v: "94138bf5b44d932443423a7eba8a9b20b6bdf33e9daf41c0fe48ba4a1ad41361",
    big_r: "94f918d7c467161ccf16f49e03541bb01c9613c1d5d9661251e4b09dc0d4df6f",
    x: "62cb64914c21ac6d6a5bb3f7aa50fb1200a3255d648cc09b1ca85a41b8cf705c",
    qty_blinding: "5bfe4512ffa637e18ca856667d1a7252f95647c6b2d62b3cb6d6123d8359e20c",
    flv_blinding: "11aab0c5db0f826c5aa4594e13c8698d66fc740bf86dac05e17834364f433f07",
    siv_key: "970f556ca15017adda4240dc7ff7e63025ee5c8e1f5d9624e2563479cb97529a",
    c_qty: "68b16eb4d12328cae5a907d543f6ff3618a30c4011a9503296d7416d1975f669",
    c_flv: "8e9dde6f51f4c670565d76810e73c5179653c576e3aec7b5f1e418add1e8943c",
    tag: "269e9e0cda8e0b63207ca6e4ef90b8d5",
    note: concat!(
        "0194f918d7c467161ccf16f49e03541bb01c9613c1d5d9661251e4b09dc0d4df",
        "6f269e9e0cda8e0b63207ca6e4ef90b8d58c3a010fde4de2174c7a96d403f52f",
        "657f63b6cb0482814fd2e993170934740d2098c2b126cf72d0",
    ),
};

fn hex32(text: &str) -> [u8; 32] {
    hex::decode(text)
        .expect("hex")
        .try_into()
        .expect("32 bytes")
}

/// Seals the vector's output from the sender's side, checks every derived
/// value against the doc, then opens the note from the recipient's side.
fn check(vector: &Vector) {
    // The zero seed is the doc's; `v` pins the derivation path before
    // anything is derived from it.
    let account = SpendAccount::from_seed(&[0u8; 64], Network::Testnet, 0).expect("account");
    let address = account.address_at(vector.branch, 0).expect("address");
    let v = account
        .viewing_key_at(vector.branch, 0)
        .expect("viewing key");
    assert_eq!(hex::encode(v.as_bytes()), vector.v, "v");
    assert_eq!(address.to_bech32(Network::Testnet), vector.address);
    let s = address.spending_key().compress();
    assert_eq!(hex::encode(s.as_bytes()), vector.s, "S");
    assert_eq!(
        hex::encode(address.viewing_key().compress().as_bytes()),
        vector.big_v,
        "V"
    );
    assert_eq!(hex::encode(FLAME_FLAVOR.as_bytes()), vector.flv, "flv");

    // Sender.
    let r = DalekScalar::from_canonical_bytes(hex32(vector.r)).expect("a canonical r");
    let sealed = seal(&address, &r, vector.qty, FLAME_FLAVOR, vector.memo);
    let secrets = &sealed.secrets;
    let big_r = RistrettoPoint::mul_base(&r).compress();
    assert_eq!(hex::encode(big_r.as_bytes()), vector.big_r, "R");
    assert_eq!(hex::encode(secrets.x.as_bytes()), vector.x, "X");
    assert_eq!(
        hex::encode(secrets.qty_blinding.as_bytes()),
        vector.qty_blinding,
        "qty_blinding"
    );
    assert_eq!(
        hex::encode(secrets.flv_blinding.as_bytes()),
        vector.flv_blinding,
        "flv_blinding"
    );
    assert_eq!(hex::encode(secrets.siv_key), vector.siv_key, "siv_key");
    let c_qty = Commitment::blinded_with_factor(Scalar::from(vector.qty), secrets.qty_blinding);
    let c_flv = Commitment::blinded_with_factor(FLAME_FLAVOR, secrets.flv_blinding);
    assert_eq!(
        hex::encode(c_qty.to_point().as_bytes()),
        vector.c_qty,
        "C_qty"
    );
    assert_eq!(
        hex::encode(c_flv.to_point().as_bytes()),
        vector.c_flv,
        "C_flv"
    );
    assert_eq!(hex::encode(&sealed.note[33..49]), vector.tag, "tag");
    assert_eq!(hex::encode(&sealed.note), vector.note, "note");
    assert_eq!(sealed.note.len(), 89 + vector.memo.len());

    // Recipient. A contract cell encodes only the commitment points, so the
    // contract rebuilt from the opening has the published contract's id.
    let token = Token::from_opening(
        Scalar::from(vector.qty),
        FLAME_FLAVOR,
        secrets.qty_blinding,
        secrets.flv_blinding,
    )
    .expect("a u64 quantity");
    let published = Contract::new(
        Predicate::opaque(s),
        Anchor([0x07; 32]),
        Value::Token(token),
    )
    .expect("a token is portable");
    let note = hex::decode(vector.note).expect("hex");
    let received =
        open_note(&published, Some(&note), &address, &v).expect("the recipient opens the note");
    assert_eq!(received.opening.qty, vector.qty);
    assert_eq!(received.opening.flv, FLAME_FLAVOR);
    assert_eq!(
        hex::encode(received.opening.qty_blinding.as_bytes()),
        vector.qty_blinding
    );
    assert_eq!(
        hex::encode(received.opening.flv_blinding.as_bytes()),
        vector.flv_blinding
    );
    assert_eq!(received.memo, vector.memo);

    // Printing what was received names the memo's length and nothing else.
    assert_eq!(format!("{:?}", received.opening), "Opening { .. }");
    assert_eq!(
        format!("{received:?}"),
        format!(
            "ReceivedNote {{ opening: Opening {{ .. }}, memo_len: {}, .. }}",
            vector.memo.len()
        )
    );
}

#[test]
fn the_payment_vector() {
    check(&PAYMENT);
}

#[test]
fn the_change_vector() {
    check(&CHANGE);
}

fn account() -> SpendAccount {
    SpendAccount::from_seed(&SEED, Network::Testnet, 0).expect("account from seed")
}

/// A cleartext allocation under `RECEIVING/0`, spent as a clear input.
fn allocation(account: &SpendAccount, qty: u64) -> InputSpec {
    let contract = Contract::new(
        account.predicate_at(util::RECEIVING, 0).expect("predicate"),
        Anchor([0x07; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(qty), FLAME_FLAVOR)),
    )
    .expect("a clear token is portable");
    let key = account.spending_key_at(util::RECEIVING, 0).expect("key");
    InputSpec::clear(contract, Proof::Transient, key).expect("clear input")
}

/// The allocation, split into `outputs` with no fee, signed and published.
fn transfer(account: &SpendAccount, outputs: &[OutputSpec], rng: &mut StdRng) -> TxLog {
    let total = outputs.iter().map(|output| output.qty).sum();
    let unsigned = build_transfer(
        &[allocation(account, total)],
        outputs,
        0,
        header(),
        limits(),
        rng,
    )
    .expect("build");
    let key = account.spending_key_at(util::RECEIVING, 0).expect("key");
    let tx = sign(unsigned, &[key]).expect("sign");
    publish(&tx).1
}

/// A contract built by hand for `address` under a known `r`, the way the
/// vectors are, with the honest note beside it.
fn by_hand(address: &ReceivingAddress, r: &DalekScalar, qty: u64) -> (Contract, Vec<u8>) {
    let sealed = seal(address, r, qty, FLAME_FLAVOR, b"");
    let token = Token::from_opening(
        Scalar::from(qty),
        FLAME_FLAVOR,
        sealed.secrets.qty_blinding,
        sealed.secrets.flv_blinding,
    )
    .expect("a u64 quantity");
    let contract = Contract::new(
        Predicate::opaque(address.spending_key().compress()),
        Anchor([0x07; 32]),
        Value::Token(token),
    )
    .expect("a token is portable");
    (contract, sealed.note.clone())
}

/// One published output to `RECEIVING/1` with its note, and the keys that
/// open it.
struct Paid {
    contract: Contract,
    note: Vec<u8>,
    address: ReceivingAddress,
    v: DalekScalar,
}

fn paid(seed: u64) -> Paid {
    let account = account();
    let address = account.address_at(util::RECEIVING, 1).expect("address");
    let log = transfer(
        &account,
        &[native_output(address, ALLOCATION)],
        &mut rng(seed),
    );
    let (contract, note) = output_to(&log, &address);
    Paid {
        contract: contract.clone(),
        note: note.expect("every output has a note").to_vec(),
        address,
        v: account.viewing_key_at(util::RECEIVING, 1).expect("key"),
    }
}

impl Paid {
    fn open(&self, note: &[u8]) -> Result<crate::note::ReceivedNote, NoteError> {
        open_note(&self.contract, Some(note), &self.address, &self.v)
    }
}

// ── The compliance checks of `payments.md` ─────────────────────────────

#[test]
fn two_outputs_to_one_address_have_different_r() {
    let account = account();
    let address = account.address_at(util::RECEIVING, 1).expect("address");
    let log = transfer(
        &account,
        &[
            native_output(address, ALLOCATION / 2),
            native_output(address, ALLOCATION - ALLOCATION / 2),
        ],
        &mut rng(30),
    );
    let notes: Vec<&[u8]> = outputs_with_notes(&log)
        .into_iter()
        .map(|(_, note)| note.expect("every output has a note"))
        .collect();
    assert_eq!(notes.len(), 2);
    assert_ne!(notes[0][1..33], notes[1][1..33], "a fresh R per output");

    // And both open: the recipient tells them apart by nothing but R.
    let v = account.viewing_key_at(util::RECEIVING, 1).expect("key");
    let mut qtys: Vec<u64> = outputs_with_notes(&log)
        .into_iter()
        .map(|(contract, note)| {
            open_note(contract, note, &address, &v)
                .expect("opens")
                .opening
                .qty
        })
        .collect();
    qtys.sort();
    assert_eq!(qtys, vec![ALLOCATION / 2, ALLOCATION - ALLOCATION / 2]);
}

#[test]
fn an_identity_or_invalid_r_is_malformed() {
    let paid = paid(31);
    paid.open(&paid.note).expect("the untouched note opens");

    let mut identity = paid.note.clone();
    identity[1..33].copy_from_slice(CompressedRistretto::default().as_bytes());
    assert_eq!(paid.open(&identity).unwrap_err(), NoteError::Malformed);

    // Not a canonical field element, so no Ristretto encoding.
    let mut invalid = paid.note.clone();
    invalid[1..33].copy_from_slice(&[0xff; 32]);
    assert!(CompressedRistretto(hex32(&"ff".repeat(32)))
        .decompress()
        .is_none());
    assert_eq!(paid.open(&invalid).unwrap_err(), NoteError::Malformed);
}

// ── One test per `NoteError` ───────────────────────────────────────────

#[test]
fn no_note_is_missing() {
    let paid = paid(32);
    assert_eq!(
        open_note(&paid.contract, None, &paid.address, &paid.v).unwrap_err(),
        NoteError::Missing
    );
}

/// Step 4's three checks, in the doc's order: empty, then the version, then
/// the length. A one-byte note is malformed or of an unknown version,
/// depending on that byte alone.
#[test]
fn a_short_note_is_malformed() {
    let paid = paid(33);
    assert_eq!(paid.open(&[]).unwrap_err(), NoteError::Malformed);
    assert_eq!(
        paid.open(&[NOTE_VERSION]).unwrap_err(),
        NoteError::Malformed
    );
    assert_eq!(
        paid.open(&paid.note[..88]).unwrap_err(),
        NoteError::Malformed,
        "88 bytes is one short of an empty memo"
    );
    assert_eq!(
        paid.open(&[0x02]).unwrap_err(),
        NoteError::UnknownVersion(0x02)
    );
}

#[test]
fn a_non_canonical_flavor_is_malformed() {
    let account = account();
    let address = account.address_at(util::RECEIVING, 2).expect("address");
    let v = account.viewing_key_at(util::RECEIVING, 2).expect("key");
    let r = DalekScalar::from(0x7e57u64);
    let (contract, _) = by_hand(&address, &r, 5_000);

    // Authentic, so only step 7 can refuse it.
    let mut plaintext = 5_000u64.to_le_bytes().to_vec();
    plaintext.extend_from_slice(&[0xff; 32]);
    let note = seal_plaintext(&address, &r, &plaintext);
    assert_eq!(
        open_note(&contract, Some(&note), &address, &v).unwrap_err(),
        NoteError::Malformed
    );
}

#[test]
fn another_version_is_unknown() {
    let paid = paid(34);
    let mut note = paid.note.clone();
    note[0] = 0x02;
    assert_eq!(
        paid.open(&note).unwrap_err(),
        NoteError::UnknownVersion(0x02)
    );
}

#[test]
fn a_flipped_ciphertext_bit_is_undecryptable() {
    let paid = paid(35);
    let mut note = paid.note.clone();
    *note.last_mut().expect("a ciphertext") ^= 0x01;
    assert_eq!(paid.open(&note).unwrap_err(), NoteError::Undecryptable);
}

#[test]
fn the_wrong_viewing_key_is_undecryptable() {
    let paid = paid(36);
    let wrong = account().viewing_key_at(util::RECEIVING, 2).expect("key");
    assert_eq!(
        open_note(&paid.contract, Some(&paid.note), &paid.address, &wrong).unwrap_err(),
        NoteError::Undecryptable
    );
}

#[test]
fn a_note_that_misstates_the_amount_is_an_opening_mismatch() {
    let account = account();
    let address = account.address_at(util::RECEIVING, 2).expect("address");
    let v = account.viewing_key_at(util::RECEIVING, 2).expect("key");
    let r = DalekScalar::from(0x0dd5u64);
    let (contract, honest) = by_hand(&address, &r, 5_000);
    open_note(&contract, Some(&honest), &address, &v).expect("the honest note opens");

    // The same key and the same blinding factors, one spark more.
    let mut plaintext = 5_001u64.to_le_bytes().to_vec();
    plaintext.extend_from_slice(FLAME_FLAVOR.as_bytes());
    let lie = seal_plaintext(&address, &r, &plaintext);
    assert_eq!(
        open_note(&contract, Some(&lie), &address, &v).unwrap_err(),
        NoteError::OpeningMismatch
    );
}

/// Step 2 comes before step 3: a clear token is refused whether or not an
/// entry follows it.
#[test]
fn a_clear_token_is_not_confidential() {
    let paid = paid(37);
    let account = account();
    let clear = Contract::new(
        account.predicate_at(util::RECEIVING, 1).expect("predicate"),
        Anchor([0x07; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(ALLOCATION), FLAME_FLAVOR)),
    )
    .expect("a clear token is portable");
    for note in [None, Some(paid.note.as_slice())] {
        assert_eq!(
            open_note(&clear, note, &paid.address, &paid.v).unwrap_err(),
            NoteError::NotConfidential
        );
    }
}

// ── Where a note may and may not travel ────────────────────────────────

#[test]
fn a_moved_note_is_undecryptable() {
    let account = account();
    let first = account.address_at(util::RECEIVING, 1).expect("address");
    let second = account.address_at(util::RECEIVING, 2).expect("address");
    let log = transfer(
        &account,
        &[
            native_output(first, ALLOCATION / 2),
            native_output(second, ALLOCATION - ALLOCATION / 2),
        ],
        &mut rng(38),
    );
    let (_, first_note) = output_to(&log, &first);
    let (second_contract, second_note) = output_to(&log, &second);
    let second_v = account.viewing_key_at(util::RECEIVING, 2).expect("key");
    open_note(second_contract, second_note, &second, &second_v).expect("its own note opens");

    assert_eq!(
        open_note(second_contract, first_note, &second, &second_v).unwrap_err(),
        NoteError::Undecryptable
    );
}

/// A wallet restored from its seed has issued nothing, and still finds and
/// opens every output it owns, change included.
#[test]
fn a_restored_wallet_opens_its_payment_and_its_change() {
    let sender = account();
    let paid_to = sender.address_at(util::RECEIVING, 3).expect("address");
    let change_to = sender.address_at(util::CHANGE, 2).expect("address");
    let memo = b"rent, september".to_vec();
    let log = transfer(
        &sender,
        &[
            OutputSpec {
                memo: memo.clone(),
                ..native_output(paid_to, 7_000_000_000)
            },
            native_output(change_to, ALLOCATION - 7_000_000_000),
        ],
        &mut rng(39),
    );

    let restored = account();
    assert_eq!(restored.next_index(), 0, "nothing issued");
    let mut found = Vec::new();
    for (contract, note) in outputs_with_notes(&log) {
        let (branch, n) = restored
            .owns(&contract.predicate.to_point(), 5)
            .expect("the restored wallet owns every output");
        let received = open_note(
            contract,
            note,
            &restored.address_at(branch, n).expect("address"),
            &restored.viewing_key_at(branch, n).expect("key"),
        )
        .expect("the restored wallet opens every note");
        found.push((branch, n, received.opening.qty, received.memo));
    }
    found.sort();
    assert_eq!(
        found,
        vec![
            (util::RECEIVING, 3, 7_000_000_000, memo),
            (util::CHANGE, 2, ALLOCATION - 7_000_000_000, Vec::new()),
        ]
    );
}

// ── The memo, the order, and the hedge ─────────────────────────────────

#[test]
fn a_memo_of_memo_max_bytes_builds_and_opens_and_one_more_does_not() {
    let account = account();
    let address = account.address_at(util::RECEIVING, 1).expect("address");
    let memo: Vec<u8> = (0..MEMO_MAX).map(|i| (i % 251) as u8).collect();
    let mut rng = rng(40);

    let longest = OutputSpec {
        memo: memo.clone(),
        ..native_output(address, ALLOCATION)
    };
    let log = transfer(&account, &[longest], &mut rng);
    let (_, note) = output_to(&log, &address);
    assert_eq!(
        note.expect("a note").len(),
        flamevm::String::MAX_LEN,
        "the longest note fills one VM String"
    );
    let (_, received) = receive(&log, &account, util::RECEIVING, 1);
    assert_eq!(received.memo, memo);

    let too_long = OutputSpec {
        memo: vec![0; MEMO_MAX + 1],
        ..native_output(address, ALLOCATION)
    };
    let Err(error) = build_transfer(
        &[allocation(&account, ALLOCATION)],
        &[too_long],
        0,
        header(),
        limits(),
        &mut rng,
    ) else {
        panic!("a memo past MEMO_MAX does not fit a note");
    };
    assert!(
        matches!(error, BuilderError::MemoTooLong(len) if len == MEMO_MAX + 1),
        "got {error:?}"
    );
}

#[test]
fn outputs_appear_in_ascending_quantity_commitment_order() {
    let account = account();
    let specs: Vec<OutputSpec> = (0..5u32)
        .map(|n| {
            native_output(
                account
                    .address_at(util::RECEIVING, 10 + n)
                    .expect("address"),
                ALLOCATION / 5 + u64::from(n),
            )
        })
        .collect();
    let mut specs = specs;
    let last = specs.last_mut().expect("five outputs");
    last.qty = ALLOCATION - (0..4).map(|n| ALLOCATION / 5 + n).sum::<u64>();
    let log = transfer(&account, &specs, &mut rng(41));

    let points: Vec<[u8; 32]> = outputs(&log)
        .iter()
        .map(|contract| match contract.payload() {
            Value::Token(token) => token.qty().to_point().to_bytes(),
            other => panic!("a transfer creates tokens, got {other:?}"),
        })
        .collect();
    let mut sorted = points.clone();
    sorted.sort();
    assert_eq!(points.len(), 5);
    assert_eq!(points, sorted, "sorted by enc(C_qty), compared as bytes");

    // Every output is followed by its note.
    assert!(outputs_with_notes(&log)
        .iter()
        .all(|(_, note)| note.is_some()));
}

/// The hedge binds the transfer's contents. From one generator state, the
/// same transfer draws the same `r` for its first output, and the same
/// input spent under another header, or to other outputs, draws another,
/// although that output pays the same address.
#[test]
fn one_generator_state_does_not_repeat_r_across_transfers() {
    let account = account();
    let address = account.address_at(util::RECEIVING, 1).expect("address");
    let other = account.address_at(util::RECEIVING, 2).expect("address");
    let r_of = |header: TxHeader, outputs: &[OutputSpec]| {
        let unsigned = build_transfer(
            &[allocation(&account, ALLOCATION)],
            outputs,
            0,
            header,
            limits(),
            &mut rng(42),
        )
        .expect("build");
        let (_, note) = output_to(unsigned.log(), &address);
        note.expect("a note")[1..33].to_vec()
    };
    let whole = [native_output(address, ALLOCATION)];
    let first = r_of(header(), &whole);
    assert_eq!(
        first,
        r_of(header(), &whole),
        "the control: the same transfer from the same state draws the same r"
    );
    let later = TxHeader {
        locktime: 1,
        ..header()
    };
    assert_ne!(first, r_of(later, &whole), "another locktime");
    let split = [
        native_output(address, ALLOCATION - 1),
        native_output(other, 1),
    ];
    assert_ne!(first, r_of(header(), &split), "other outputs");
}
