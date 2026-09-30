//! Encrypted payment notes: sealing one for an output, and opening it again.
//!
//! This module implements `docs/payments.md` — "Derivations", "The note" and
//! "Receiving" — and restates none of it. In short: the sender draws `r`,
//! publishes `R = r*G`, and reaches `X = r*V` against the recipient's
//! viewing key; the recipient reaches the same `X` as `v*R`. Two transcripts
//! over `R`, `S`, `V` and `X` give the output's two blinding factors and the
//! AES-128-SIV key of its note, and the note rides in the `Data` entry
//! directly after its `Output`.
//!
//! What leaves this module is an [`Opening`] and a memo, or the note bytes.
//! No public function returns `X`, `v`, the note key or a blinding factor on
//! its own: `payments.md` "Disclosure" hands out an opening, never the
//! values it was derived from.

use core::fmt;

use aes_siv::siv::Aes128Siv;
use aes_siv::KeyInit;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar as DalekScalar;
use curve25519_dalek::traits::IsIdentity;
use flamekd::ReceivingAddress;
use flamevm::{Contract, Scalar, Token, TxEntry, TxLog, Value};
use merlin::Transcript;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::builder::Opening;

/// Byte 0 of every note this module writes.
pub const NOTE_VERSION: u8 = 0x01;

/// The longest memo a note carries: a note is `89 + N` bytes and travels as
/// one VM `String`, whose limit is 8191 bytes.
pub const MEMO_MAX: usize = 8102;

/// The version byte, `enc(R)` and the SIV tag: everything in front of the
/// ciphertext.
const HEADER_LEN: usize = 1 + 32 + 16;

/// `LE64(qty) ++ enc(flv)`: the plaintext in front of the memo.
const AMOUNT_LEN: usize = 8 + 32;

/// The shortest note: a version-1 note with an empty memo.
const NOTE_MIN: usize = HEADER_LEN + AMOUNT_LEN;

/// What opening a note gives its recipient.
///
/// Its `Debug` output names the memo's length and nothing else: the amount,
/// both blinding factors and the memo stay out of logs.
#[derive(Clone)]
pub struct ReceivedNote {
    /// What [`crate::InputSpec::confidential`] spends the output with.
    pub opening: Opening,
    /// The memo, as the sender wrote it.
    pub memo: Vec<u8>,
}

impl fmt::Debug for ReceivedNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceivedNote")
            .field("opening", &self.opening)
            .field("memo_len", &self.memo.len())
            .finish_non_exhaustive()
    }
}

/// Why a note did not open. The variants are the results of `payments.md`
/// "Receiving", plus [`NoteError::NotConfidential`] for an output that has
/// nothing to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NoteError {
    /// No `Data` entry follows the output.
    #[error("no note follows the output")]
    Missing,

    /// The note is too short, its `R` is invalid or the identity, or its
    /// flavor is not a canonical scalar.
    #[error("the note is malformed")]
    Malformed,

    /// A note format this crate does not know; a later version may open it.
    #[error("note version {0:#04x} is unknown")]
    UnknownVersion(u8),

    /// The note does not authenticate under this address's key: it was
    /// sealed for someone else, moved, or altered.
    #[error("the note does not decrypt under this address")]
    Undecryptable,

    /// The note decrypts but does not describe the output it follows: its
    /// sender erred or lied.
    #[error("the note does not describe the output it follows")]
    OpeningMismatch,

    /// The output's payload is not a `Token`; there is nothing to open.
    #[error("the output holds no confidential Token")]
    NotConfidential,
}

/// Opens the note that followed `published` in its log. `address` and
/// `view_key` are the recipient's `(S_n, V_n)` and `v_n`; the caller has
/// already matched the output's predicate to `S_n`.
///
/// This is `payments.md` "Receiving", steps 2 to 9, in order. Step 8 is made
/// the way [`crate::InputSpec::confidential`] makes it: the contract is
/// rebuilt from the opening and the two ids are compared.
pub fn open_note(
    published: &Contract,
    note: Option<&[u8]>,
    address: &ReceivingAddress,
    view_key: &DalekScalar,
) -> Result<ReceivedNote, NoteError> {
    // 2. Only a `Token` has a note to open.
    if !matches!(published.payload(), Value::Token(_)) {
        return Err(NoteError::NotConfidential);
    }

    // 3.
    let note = note.ok_or(NoteError::Missing)?;

    // 4. The version first, because a later version may have another
    //    length.
    match note.first() {
        None => return Err(NoteError::Malformed),
        Some(&NOTE_VERSION) => {}
        Some(&version) => return Err(NoteError::UnknownVersion(version)),
    }
    if note.len() < NOTE_MIN {
        return Err(NoteError::Malformed);
    }

    // 5.
    let r_bytes = CompressedRistretto::from_slice(&note[1..33])
        .expect("a 32-byte slice is a compressed point's length");
    let r_point = r_bytes.decompress().ok_or(NoteError::Malformed)?;
    if r_point.is_identity() {
        return Err(NoteError::Malformed);
    }

    // 6. `S_n` is the output's predicate; the caller matched it.
    let x = shared_point(view_key, &r_point);
    let secrets = Secrets::derive(
        &r_bytes,
        &published.predicate.to_point(),
        &address.viewing_key().compress(),
        &x,
    );
    let plaintext = Zeroizing::new(
        secrets
            .cipher()
            .decrypt(core::iter::empty::<&[u8]>(), &note[33..])
            .map_err(|_| NoteError::Undecryptable)?,
    );

    // 7.
    let qty = u64::from_le_bytes(
        plaintext[..8]
            .try_into()
            .expect("the length check leaves eight bytes of quantity"),
    );
    let flv = Scalar::from_bytes(
        plaintext[8..AMOUNT_LEN]
            .try_into()
            .expect("the length check leaves 32 bytes of flavor"),
    )
    .ok_or(NoteError::Malformed)?;
    let memo = plaintext[AMOUNT_LEN..].to_vec();

    // 8.
    let opening = Opening {
        qty,
        flv,
        qty_blinding: secrets.qty_blinding,
        flv_blinding: secrets.flv_blinding,
    };
    let token = Token::from_opening(
        Scalar::from(qty),
        flv,
        opening.qty_blinding,
        opening.flv_blinding,
    )
    .expect("a u64 quantity is always inside the 64-bit range");
    let rebuilt = Contract::new(
        published.predicate.to_opaque(),
        published.anchor,
        Value::Token(token),
    )
    .map_err(|_| NoteError::OpeningMismatch)?;
    if rebuilt.id() != published.id() {
        return Err(NoteError::OpeningMismatch);
    }

    // 9.
    Ok(ReceivedNote { opening, memo })
}

/// Every output of a log with its note: the `Data` entry immediately after
/// it, if there is one.
pub fn outputs_with_notes(log: &TxLog) -> Vec<(&Contract, Option<&[u8]>)> {
    let entries = log.entries();
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            TxEntry::Output(contract) => {
                let note = match entries.get(index + 1) {
                    Some(TxEntry::Data(bytes)) => Some(bytes.as_slice()),
                    _ => None,
                };
                Some((contract, note))
            }
            _ => None,
        })
        .collect()
}

/// What one output's shared point derives, and the point itself.
///
/// Only ever held in a `Box`, derived in place: moving or sorting a
/// `Box<Secrets>` moves a pointer, so the one copy of these values is the
/// heap allocation this type zeroizes on drop. It is never handed out
/// whole: a blinding factor leaves this crate only inside an [`Opening`].
///
/// What this cannot wipe is what flamevm holds. `Token::from_opening` and
/// `Commitment::blinded_with_factor` keep the blinding factors in a
/// `CommitmentWitness`, which does not zeroize, so every commitment built
/// from these secrets leaves a copy behind when flamevm drops it.
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct Secrets {
    pub(crate) x: CompressedRistretto,
    pub(crate) siv_key: [u8; 32],
    pub(crate) qty_blinding: DalekScalar,
    pub(crate) flv_blinding: DalekScalar,
}

impl Secrets {
    /// Both derivations of `payments.md` "Derivations", over the output's
    /// `R`, the recipient's `(S, V)`, and `X`.
    pub(crate) fn derive(
        r: &CompressedRistretto,
        s: &CompressedRistretto,
        v: &CompressedRistretto,
        x: &CompressedRistretto,
    ) -> Box<Secrets> {
        let mut secrets = Box::new(Secrets {
            x: CompressedRistretto::default(),
            siv_key: [0u8; 32],
            qty_blinding: DalekScalar::ZERO,
            flv_blinding: DalekScalar::ZERO,
        });
        secrets.x = *x;
        secrets.derive_blinding_factors(r, s, v);
        secrets.derive_note_key(r, s, v);
        secrets
    }

    /// `flame.blinding`: the output's quantity and flavor blinding factors.
    fn derive_blinding_factors(
        &mut self,
        r: &CompressedRistretto,
        s: &CompressedRistretto,
        v: &CompressedRistretto,
    ) {
        let mut transcript = bound(b"flame.blinding", r, s, v, &self.x);
        self.qty_blinding = challenge_scalar(&mut transcript, b"qty");
        self.flv_blinding = challenge_scalar(&mut transcript, b"flv");
    }

    /// `flame.notekey.v1`: the note's AES-128-SIV key, CMAC half first,
    /// written straight into this allocation.
    fn derive_note_key(
        &mut self,
        r: &CompressedRistretto,
        s: &CompressedRistretto,
        v: &CompressedRistretto,
    ) {
        let mut transcript = bound(b"flame.notekey.v1", r, s, v, &self.x);
        transcript.challenge_bytes(b"siv_key", &mut self.siv_key);
    }

    /// The note's cipher, keyed from this allocation without copying the
    /// key out of it. The cipher wipes its own key schedule on drop.
    fn cipher(&self) -> Aes128Siv {
        Aes128Siv::new_from_slice(&self.siv_key).expect("the note key is 32 bytes")
    }
}

/// `X = k*P`, compressed. The uncompressed product is wiped too.
fn shared_point(k: &DalekScalar, point: &RistrettoPoint) -> Zeroizing<CompressedRistretto> {
    let product = Zeroizing::new(k * point);
    Zeroizing::new(product.compress())
}

/// A transcript that has bound `R`, `S`, `V` and `X`, in that order.
fn bound(
    label: &'static [u8],
    r: &CompressedRistretto,
    s: &CompressedRistretto,
    v: &CompressedRistretto,
    x: &CompressedRistretto,
) -> Transcript {
    let mut transcript = Transcript::new(label);
    transcript.append_message(b"R", r.as_bytes());
    transcript.append_message(b"S", s.as_bytes());
    transcript.append_message(b"V", v.as_bytes());
    transcript.append_message(b"X", x.as_bytes());
    transcript
}

/// flamekd's `challenge_scalar`: 64 challenge bytes, reduced.
fn challenge_scalar(transcript: &mut Transcript, label: &'static [u8]) -> DalekScalar {
    let mut wide = Zeroizing::new([0u8; 64]);
    transcript.challenge_bytes(label, wide.as_mut());
    DalekScalar::from_bytes_mod_order_wide(&wide)
}

/// One sealed output: its note, and the secrets of the commitments the note
/// describes.
pub(crate) struct Sealed {
    pub(crate) note: Vec<u8>,
    pub(crate) secrets: Box<Secrets>,
}

/// Seals the note of an output that pays `qty` of `flv` to `address`, under
/// the sender's `r`. The caller draws `r` fresh and nonzero for every output
/// (`payments.md` "Sender randomness") and bounds the memo by [`MEMO_MAX`].
pub(crate) fn seal(
    address: &ReceivingAddress,
    r: &DalekScalar,
    qty: u64,
    flv: Scalar,
    memo: &[u8],
) -> Sealed {
    let mut plaintext = Zeroizing::new(Vec::with_capacity(AMOUNT_LEN + memo.len()));
    plaintext.extend_from_slice(&qty.to_le_bytes());
    plaintext.extend_from_slice(flv.as_bytes());
    plaintext.extend_from_slice(memo);
    let (r_bytes, secrets) = exchange(address, r);
    Sealed {
        note: encrypt(&r_bytes, &secrets, &plaintext),
        secrets,
    }
}

/// Seals an arbitrary plaintext under the key of the output that `r` pays
/// to `address`: a note no honest sender writes.
#[cfg(test)]
pub(crate) fn seal_plaintext(
    address: &ReceivingAddress,
    r: &DalekScalar,
    plaintext: &[u8],
) -> Vec<u8> {
    let (r_bytes, secrets) = exchange(address, r);
    encrypt(&r_bytes, &secrets, plaintext)
}

/// The sender's half of the exchange: `R = r*G`, `X = r*V`, and what they
/// derive for this address.
fn exchange(address: &ReceivingAddress, r: &DalekScalar) -> (CompressedRistretto, Box<Secrets>) {
    debug_assert!(*r != DalekScalar::ZERO, "r must be nonzero");
    let r_bytes = RistrettoPoint::mul_base(r).compress();
    let x = shared_point(r, address.viewing_key());
    let secrets = Secrets::derive(
        &r_bytes,
        &address.spending_key().compress(),
        &address.viewing_key().compress(),
        &x,
    );
    (r_bytes, secrets)
}

/// `0x01 ++ enc(R) ++ tag ++ ciphertext`.
fn encrypt(r_bytes: &CompressedRistretto, secrets: &Secrets, plaintext: &[u8]) -> Vec<u8> {
    let sealed = secrets
        .cipher()
        .encrypt(core::iter::empty::<&[u8]>(), plaintext)
        .expect("with no associated data, AES-SIV encryption cannot fail");
    let mut note = Vec::with_capacity(1 + 32 + sealed.len());
    note.push(NOTE_VERSION);
    note.extend_from_slice(r_bytes.as_bytes());
    note.extend_from_slice(&sealed);
    note
}
