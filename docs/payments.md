# Confidential payments

This document specifies how a wallet pays a Receiving Address with a
confidential token: how the sender builds each output and the encrypted note
that travels with it, where the note sits in the transaction log, how the
recipient finds and opens it, and how the recipient later spends the output.

The words MUST, MUST NOT, SHOULD, and MAY specify implementation requirements
for wallets and nodes that interoperate. None of them is a consensus rule; see
[Scope and trust boundary](#scope-and-trust-boundary).

## Status

Proposed; not implemented. Today `flamewallet::build_transfer` takes
caller-chosen blinding factors and emits no note, and `flamed`'s `scan`
returns a contract's bytes without the entry that follows it. This document
is the target for both.

The protocol follows the requirements `flamekd.md` sets for token encryption;
[Compliance with flamekd](#compliance-with-flamekd) maps each of them to the
section that meets it.

## Scope and trust boundary

This document specifies what wallets and nodes agree on off chain. Nothing in
it is consensus. The chain validates a payment like any other transaction:
its script, its `mix` range proofs, and its signature. To the chain a note is
an ordinary `Data` entry, and no validator parses, checks, or decrypts one. A
note that is malformed, or that misstates its output, is valid on chain; only
its recipient can tell ([Receiving](#receiving)). That is why this document
uses the uppercase keywords of `flamekd.md`: `flamevm.md` reserves bold
**must** for consensus behavior, and this document states none.

| Party | Holds | Does |
| --- | --- | --- |
| Sender | the recipient's address and `r` | Builds the output and its note. |
| Node | no keys | Keeps history, and returns each output with the entry that follows it, byte for byte. |
| Recipient | `v_n` and `s_n` | Finds, opens, checks, and spends the output. |

A node can withhold an output or its note. A wallet that does not check an
output's membership proof against a chain state it trusts also relies on the
node for the output's existence: a note that opens proves only that its
writer knew `V`, and anyone given the address knows `V`.

## Notation

This document uses the notation and the transcript conventions of
[`flamekd.md`](../flamekd/flamekd.md#transcript-conventions): `G` is the
ristretto255 generator, `x*P` is scalar multiplication, `enc(x)` and `enc(P)`
are the canonical 32-byte encodings of a scalar and a compressed point, `++`
is concatenation, and `Transcript(domain)`, `P.append(label, bytes)` and
`P.challenge_scalar(label)` are the Merlin operations defined there.
`P.challenge_bytes(label, n)` is Merlin's `challenge_bytes` asked for exactly
`n` bytes. Merlin binds the requested length into its output, so a longer
challenge cut down to `n` bytes is a different value.

`LE64(q)` is the 8-byte little-endian encoding of an unsigned 64-bit integer.

An address in this document is always a `flamekd.md` address: a Receiving
Address `(S, V)` or a Tracking address `(S)`. It is never the FlameVM
`Address` type ([`flamevm.md`](flamevm.md#addresses)), which names a send
target, a predicate or an actor message.

`Com(x, b) = x*G + b*B̃` is a Pedersen commitment over flamevm's generators,
`PedersenGens::default()` from `bulletproofs`: its base point is `G`, and
`B̃` is its blinding generator. A confidential `Token` holds two of them, one
to the quantity and one to the flavor.

## Overview

A Receiving Address is two public keys, `(S, V)`
([`flamekd.md`](../flamekd/flamekd.md#key-types-and-capabilities)). `S` locks
the payment: it becomes the output's predicate, and only its secret `s` can
authorize a spend. `V` encrypts the payment: the sender runs a
Diffie-Hellman exchange against it, and only its secret `v` recovers the
shared secret.

For each output, the sender draws a fresh scalar `r` and publishes `R = r*G`.
Both sides reach the same point `X`, the sender as `r*V` and the recipient as
`v*R`. Two transcripts over `X`, bound to `R` and to the address, give the
output's two blinding factors and a key for its note. The note carries the
quantity, the flavor, and a memo, encrypted under that key, and is logged
directly after the output:

```text
Header · CellWitness · Input(id)… · Fee(fee)
Output(Contract { enc(S_a), anchor, Token { C_qty, C_flv } })   Data(0x01 ++ enc(R_0) ++ tag_0 ++ ct_0)
Output(Contract { enc(S_b), anchor, Token { C_qty, C_flv } })   Data(0x01 ++ enc(R_1) ++ tag_1 ++ ct_1)
```

The recipient finds its outputs by predicate, computes `X` from `R`, decrypts
the note, rebuilds both commitments from what it read, and compares them with
the published ones. Because every secret of the output derives from `X` and
public values, the recipient never needs the sender to deliver anything out
of band, and a wallet restored from its seed recovers every output it owns,
change included, from any node that has kept the chain's history
([Restoring from a seed](#restoring-from-a-seed)).

## The Receiving Address

The output's predicate is `Predicate::opaque(enc(S))`: the bare public
spending key, with no Taproot tree behind it. A spend authorizes by signing
the transaction with `signtx`
([`flamewallet.md`](../flamewallet/flamewallet.md#an-address-and-a-predicate)).

`S` appears on chain; `V` never does. The predicate is `S` itself, not a
one-time key derived per payment, so a node can index outputs by predicate
and a wallet can ask for its own. The cost is linkability: two payments to
one address show the same predicate, although their amounts stay hidden.
Wallets SHOULD hand out a fresh address for each payment.

A Tracking address carries only `S`. With no `V` there is nothing to encrypt
to, so it can receive only clear tokens, which carry no note and are outside
this document.

## Sender randomness

For each output, the sender MUST draw `r` uniformly at random from the
nonzero scalars. It MUST NOT reuse `r` for another output, including an
output of a transaction that never confirms, and MUST NOT choose `r` with a
known relation to any other `r`, such as `r_i = r_0 + i`. A sender SHOULD
hedge `r` against a weak system random number generator, for example with a
Merlin `TranscriptRng` rekeyed with the transaction's signing keys and
finalized with the system generator.

| Failure | Consequence |
| --- | --- |
| The same `r` for two outputs to one address | The same `X`, so the same blinding factors. `C_1 - C_2 = (q_1 - q_2)*G` shows whether the amounts are equal, and a 64-bit difference falls to a kangaroo search in about 2^32 steps. Disclosing one output discloses the other. |
| A predictable `r` | Anyone computes `X = r*V`: the amount, the flavor, and the memo are public. |
| An `r` with a known relation to another | Anyone holding one `X` computes the other. |

## The shared secret

```text
R = r*G
X = r*V      (sender)
X = v*R      (recipient)
```

`V` comes from an address decoded as `flamekd.md` requires, so it is valid
and not the identity. The recipient MUST decode `R` with Ristretto decoding
and MUST reject an encoding that is invalid or decodes to the identity. With
`v` nonzero and a prime-order group, `X` is then never the identity.

## Derivations

Each transcript is named for what it derives.

```text
P = Transcript("flame.blinding")
P.append("R", enc(R))
P.append("S", enc(S))
P.append("V", enc(V))
P.append("X", enc(X))
qty_blinding = P.challenge_scalar("qty")
flv_blinding = P.challenge_scalar("flv")

K = Transcript("flame.notekey.v1")
K.append("R", enc(R))
K.append("S", enc(S))
K.append("V", enc(V))
K.append("X", enc(X))
siv_key = K.challenge_bytes("siv_key", 32)
```

All appends and challenges occur in the stated order on the same transcript.

Both transcripts bind the output and its recipient before the shared secret:
`R` is the output's one-time public key, and `(S, V)` is the Receiving
Address it pays. Both sides hold every input. The sender has `R` from its
`r` and `(S, V)` from the address. The recipient reads `R` from the note,
`S` from the output's predicate, and `V` from its own address.

`flame.blinding` carries no version, on purpose. The blinding factors are
fixed into commitment points on chain the moment an output is created, so
this derivation can never change for an existing output. A different
blinding derivation needs a different label, and wallets keep this one for
as long as any output made with it can be unspent.

`flame.notekey.v1` carries the note version, `0x01`. A note whose version
byte has been changed selects a different key and fails authentication, so
the version needs no other binding.

## Commitments

```text
C_qty = Com(qty, qty_blinding)
C_flv = Com(flv, flv_blinding)
```

`C_qty` and `C_flv` are the two halves of the output's `Token`. In
`flamevm` they are `Commitment::blinded_with_factor(qty, qty_blinding)` and
`Commitment::blinded_with_factor(flv, flv_blinding)`. `qty` is a count of
the token's smallest unit, sparks for native Flame, and the native flavor is
`FLAME_FLAVOR = 1`
([`flamevm.md`](flamevm.md#tokens)).

## The note

The plaintext is:

```text
plaintext = LE64(qty) ++ enc(flv) ++ memo
```

The memo is arbitrary bytes. It has no length prefix and no padding.

The note is encrypted with AES-128-SIV
([RFC 5297](https://www.rfc-editor.org/rfc/rfc5297)) under `siv_key`: the
first 16 bytes are the CMAC key and the last 16 bytes the CTR key. There is
no associated data: the SIV is computed over zero header strings and the
plaintext. The output is `tag ++ ciphertext`, the 16-byte synthetic IV
followed by a ciphertext as long as the plaintext. This is what
`aes_siv::siv::Aes128Siv::encrypt` returns in the RustCrypto `aes-siv` crate.

The note is:

| Offset | Size | Field | |
| ---: | ---: | --- | --- |
| 0 | 1 | version, `0x01` | clear |
| 1 | 32 | `enc(R)` | clear |
| 33 | 16 | SIV tag | clear |
| 49 | 8 | `LE64(qty)` | encrypted |
| 57 | 32 | `enc(flv)` | encrypted |
| 89 | N | memo | encrypted |

A note is `89 + N` bytes, and the memo's length is public: `N` is the note's
length minus 89. The memo MUST be at most 8102 bytes. The note travels as a
single VM `String`, whose limit is one Cell payload, 8191 bytes
(`String::MAX_LEN` in `flamevm/src/string.rs`, which flamevm marks as
temporary).

## Building the transaction

The script extends the transfer in
[`flamewallet.md`](../flamewallet/flamewallet.md#a-transfer) by one
`push_str` and one `log` after each `output`:

```text
per input:    push_str(String::contract(c))  input  signtx
fee > 0:      push_int(fee)  fee
per output:   push_str(commitment(qty, qty_blinding))
              push_str(commitment(flv, flv_blinding))
              push_int(m)  push_int(n)  mix
per output i: roll_k(n-1-i) if > 0
              push_point(enc(S_i))  output
              push_str(note_i)  log
```

`push_str(note) log` leaves the stack as it found it, so the roll depths
before each `output` are unchanged.

### Pairing

The note of an output is the `Data` entry immediately following that
`Output` in the same `TxLog`. No other entry is a note.

A sender MUST place a note directly after every output with a `Token`
payload that the payment creates, the change included. Position then says
nothing about which output is the payment: every confidential output has a
note in the same place. If only payments carried notes, the notes would mark
them.

An output whose payload is a `ClearToken` carries no note, and a recipient
ignores whatever entry follows it. Outputs locked by other predicates, such
as actors or script branches, are outside this document.

### Output order

A sender SHOULD order a payment's outputs by `enc(C_qty)`, compared as bytes.
Under a fresh blinding factor that encoding is uniformly distributed, so the
order says nothing about which output is the change. A recipient never
relies on an output's position.

### On the wire

An `ExternalTx` carries a pruned `TxLog`
([`encoding.md`](encoding.md#transactions-and-execution-witnesses)). The note
travels once, as the `push_str` literal in the script, and its `Data` entry
is derived again when a verifier executes the script. The TxID covers the
`Data` entry, so the `signtx` signature covers the note: nobody but the
signer can alter it. `log` charges gas for every byte it writes: a note is
byte-sized growth, which the "Execution memory" rule under
[Storage](flamevm.md#storage) charges before allocation (`op_log` in
`flamevm/src/vm.rs`).

## Receiving

A wallet processes each `Output` of a transaction's log as follows:

1. If the predicate point is not `enc(S_n)` of an address the wallet owns,
   skip the output. Otherwise note which `(branch, n)` it is.
2. If the payload is a `ClearToken`, the output is owned and open; it needs
   no note. If the payload is a `Token`, continue. Any other payload is
   outside this document.
3. Take the entry immediately following the `Output`. If there is none, or
   it is not `Data`, the result is **no note**.
4. If the note is empty, the result is **malformed**. If byte 0 is not
   `0x01`, the result is **unknown version**; a later version may have
   another length. If the note is shorter than 89 bytes, the result is
   **malformed**.
5. Decode `R` from bytes 1..33. If it is invalid or the identity, the result
   is **malformed**.
6. Compute `X = v_n*R`, then `siv_key` from `R`, `S_n` (the output's
   predicate), `V_n`, and `X`. Decrypt bytes 33.. with AES-128-SIV and no
   associated data. If authentication fails, the result is
   **undecryptable**.
7. Read `qty` from plaintext bytes 0..8, `flv` from bytes 8..40, and the memo
   from the rest. If `flv` is not a canonical scalar encoding, the result is
   **malformed**.
8. Compute both blinding factors from the same inputs. If
   `Com(qty, qty_blinding)` is not the published `C_qty`, or
   `Com(flv, flv_blinding)` is not the published `C_flv`, the result is
   **opening mismatch**.
9. Otherwise the output is owned and open. Keep its opening
   `(qty, flv, qty_blinding, flv_blinding)` and its memo.

`flamewallet` makes the check in step 8 by rebuilding the contract with
`Token::from_opening` and comparing contract ids, as
`InputSpec::confidential` does. The two are equivalent: with the predicate
and anchor fixed, a contract's id covers exactly the two commitment points.

| Result | Meaning | Wallet |
| --- | --- | --- |
| open | The note matches the output. | Counts toward the balance. |
| no note, malformed, undecryptable | The output is ours but cannot be read. | Keep and list it; exclude it from the balance. |
| unknown version | A newer format. | Keep and list it; exclude it from the balance. A wallet that knows the version can open it. |
| opening mismatch | The note decrypts but does not describe the output: the sender erred or lied. | Keep and flag it; never count it. |

### Restoring from a seed

Nothing in this procedure depends on state the wallet had to save: every
input is derived from the seed or was published. A wallet restored from its
seed re-derives its addresses, asks a node for their outputs, and recovers
every opening and memo. Two things bound that promise.

**History.** Outputs and their notes live in transaction logs, and a
validating node does not keep them. A `Data` entry is logging that "does not
occupy permanent storage"
([`flamevm.md`](flamevm.md#external-transactions)), and a validating node
keeps the contract accumulator, not history
([`blockchain.md`](blockchain.md#contracts-and-utreexo)). Restoring needs an
archival source, a node that keeps the blocks or the full logs, as `flamed`
does. That source must keep, for every unspent output, the note that follows
it ([The node](#the-node)).

**Lookahead.** A wallet finds only outputs to the addresses it derives. How
far past its last used index a wallet looks is outside this specification,
as it is outside `flamekd.md`'s. A payment to an address beyond that range
stays unfound until the wallet looks further.

## Spending

A spend publishes nothing about the amount it spends. It follows the witness
path described in
[`flamewallet.md`](../flamewallet/flamewallet.md#why-inputs-travel-as-witnesses):

1. Rebuild the contract with open commitments from the opening:
   `Token::from_opening(qty, flv, qty_blinding, flv_blinding)` inside
   `Contract::new(opaque(S_n), anchor, …)`. Its id MUST equal the published
   id.
2. Push it with `push_str(String::contract(c))`, then `input` and `signtx`.
   The bytecode holds only the 32-byte contract id; the contract's public
   body goes into the transaction's witness bag, where its token is two
   points again; the openings stay with the prover.
3. Authorize with `s_n`, the spending scalar of the address, through the
   transaction's `signtx` aggregate signature.

On chain the spend is `TxEntry::Input(ContractID)`, and the input's Utreexo
membership proof travels in `BlockTx.proofs`. A spender MUST NOT open the
token with `decrypt`, which takes the quantity and both blinding factors as
script literals that every verifier re-executes.

A spend needs neither the note nor `siv_key`: the opening and `s_n` are
enough.

## The node

A node indexes outputs by predicate. For each `Output` it records the entry
immediately following it, if that entry is `Data`, byte for byte, and
returns it with the output: `ScanEntry.note` in `flamed-rpc`, base64 like the
contract envelope. The node does not parse, check, or decrypt notes; it holds
no keys.

A node that prunes history and still answers `scan` MUST keep, with every
unspent output it keeps, the note that follows it. Without the note the
output can be found but not opened, and a wallet restoring from its seed
cannot recover it.

`flamed` rebuilds its indexes from its block archive at startup
([`flamed.md`](../flamed/flamed.md#what-the-node-keeps-that-the-chain-forgets)),
so recording notes needs no migration of existing data directories.

## Privacy

Hidden from everyone but the sender and the recipient:

- the quantity and flavor of every output;
- the memo's contents;
- the quantity and flavor of every spent input.

Visible to everyone:

- the predicate each output pays, and so every pair of payments to one
  address;
- the transaction graph: which contracts a transaction spends and creates;
- the number of outputs;
- the memo's length;
- the fee, which is cleartext.

## Disclosure

A sender or a recipient can show a third party what one output holds.

| Disclosed | Size | The holder can | Beyond this output |
| --- | ---: | --- | --- |
| Opening `(qty, flv, qty_blinding, flv_blinding)` | 104 bytes | Check the quantity and flavor against the published commitments. | Nothing. The memo stays private, and the check needs neither the note nor its version. |
| `siv_key` | 32 bytes | Read the note, including the memo. | Nothing, but the holder cannot check the amount against the commitments. |
| `siv_key` and both blinding factors | 96 bytes | Read the note and check it. | Nothing. |
| `X` | 32 bytes | Everything above. | Every output whose `r` was reused or related, and anything else ever derived from this exchange. A recipient that computes `v*R` on request for any `R` offered to it also answers Diffie-Hellman queries on `v`. |

To prove an amount, disclose the opening; add `siv_key` only when the memo
is part of what is being proved. Never disclose `X`, as `flamekd.md`
requires. None of these values allows spending, which needs `s_n`.

A sender that may need to prove a payment later must keep what it will
disclose. It cannot recompute any of it: `r` was random, and `X` needs `v`.

## Versioning

Byte 0 of a note is its version, and this document defines `0x01`. A later
version gets a new byte and a new note key label, `flame.notekey.vN`, and MAY
keep `flame.blinding`.

A note never changes version. A `0x01` note stays one until its output is
spent, so wallets MUST keep decoding `0x01` notes while any `0x01` output
can be unspent, which in practice is forever. A wallet that meets a version
it does not know keeps the output as **unknown version**.

## Compliance with flamekd

`flamekd.md` leaves token encryption to another specification and sets its
requirements in
[Compromise boundaries](../flamekd/flamekd.md#compromise-boundaries):

> An encryption protocol SHOULD use fresh sender randomness per output,
> validated group inputs, and a domain-separated KDF bound to the output and
> recipient […] For selective disclosure, share the final key for the
> selected encrypted output, not a viewing scalar or the raw DH point.

| flamekd requirement | Met in | How |
| --- | --- | --- |
| Fresh sender randomness per output | [Sender randomness](#sender-randomness) | `r` MUST be uniform, nonzero, and fresh for every output, never reused or related to another `r`. |
| Validated group inputs | [The shared secret](#the-shared-secret) | `V` comes from a decoded address; the recipient rejects an `R` that is invalid or the identity. |
| A domain-separated KDF | [Derivations](#derivations) | Two transcripts with distinct labels, one for the blinding factors and one for the note key. |
| Bound to the output and recipient | [Derivations](#derivations) | Both transcripts append `R`, `S`, and `V` before `X`. |
| Disclose the final key, not a viewing scalar or the raw DH point | [Disclosure](#disclosure) | The opening, or `siv_key`; never `v` or `X`. |

The binding follows
[HPKE](https://www.rfc-editor.org/rfc/rfc9180.html#section-4.1), whose
key schedule takes the sender's ephemeral key and the recipient's public key
alongside the shared secret. In ristretto255 no known attack needs it: the
group has prime order and one encoding per point, so `v*R' = v*R` only when
`R' = R`. It is kept because it makes the construction the one HPKE's
analysis covers, and because it keeps keys distinct if a later version
shares one `R` across outputs or one `V` across predicates. It does not
replace the other rows: an `r` reused toward one address repeats `R`, `S`,
and `V` along with `X`.

An implementation shows compliance with four checks: two outputs to one
address get different `R`; an identity or invalid `R` is malformed; the
[test vectors](#test-vectors), which fix every transcript input and its
order, are reproduced exactly; and no disclosure interface returns `v` or
`X`.

## Prior art

Non-normative. Slingshot's address protocol (`accounts/src/address.rs` in
the Slingshot repository) is the closest predecessor. It runs the same
exchange against an address's encryption key and derives both blinding
factors through one transcript bound to the address, which also yields XOR
pads in place of a cipher. Its 73-byte `data` entry has no
version, no memo, and no MAC; a 1-byte keyed distinguisher filters
candidates, and the commitments are the only integrity check. Only payments
to an address carried an entry: change used receivers whose blinding factors
depend on the amount, so a seed alone could not recover it. The entries were
therefore sorted among all of the transaction's data entries, because an
entry next to its output would have marked the payment.

This document puts a note on every confidential output, change included,
which is what makes both the adjacent placement and restoring from a seed
possible, and it adds a MAC, an encrypted memo, and a version byte.

## Test vectors

These vectors use a binary seed of 64 zero bytes on testnet, account 0. This
is public test material, not a seed to use for funds. `r` is given as its
canonical encoding. Notes are split at 32-byte boundaries; concatenate the
lines without whitespace.

### Payment with a memo

Recipient `m/35263'/1'/0'/0/0`, Receiving Address:

```
tf1psxy35zgz289wuj33pm05vkl3sr40xp3ecdacw0dmuzu688wufwaf55kjxcgtw9v6tf5mvta3nk93nj8knx8phhrezer3n8999crggcz0p20m
```

Inputs:

```
v     7b4ac2f41156ceb0d35a9631311fe799b2f5c258e36d4f7da7b36d0599ecf804
r     0101010101010101010101010101010101010101010101010101010101010101
qty   40000000000
flv   0100000000000000000000000000000000000000000000000000000000000000
memo  "invoice 42" (10 bytes)
```

Derived values:

```
S             0c0c48d048128e5772518876fa32df8c07579831ce1bdc39eddf05cd1ceee25d
V             d4d29691b085b8acd2d34db17d8cec58ce47b4cc70dee3c8b238cce529703423
R             3e440469a098036d89ffb2d77a4542928f2f74c2b5769da7480736ace829dc10
X             fa3c8a0c613bb5c159d74ca95045846a59c443fa5897c3b4645a0ecb2980a836
qty_blinding  a4c09913a10fc47b6b6a3ba823b078e20808900d822758d31dfe8f9c3a39df0b
flv_blinding  414e5f225386ac298f99e42f858b307dd0b569ba0b2139f50975209d05712b0f
siv_key       fcbfb5bcb360103d22a8e7601bc81948f4621f170cbd9f433fdc15fecf6c3ff7
C_qty         8c5d58713758003e3a1bec088486046f59bf3dad7219a1633469d0bf9bbb6700
C_flv         38185a383a8984641e76d7490e98528beeddd4c25348f1d95e96353c25a9e51f
tag           d47cb53a92befd875389aa7600883e5c
```

Note (99 bytes):

```
013e440469a098036d89ffb2d77a4542928f2f74c2b5769da7480736ace829dc
10d47cb53a92befd875389aa7600883e5ceaa9693d564bfa2749451ac4a5acdc
1c8d458ab3cecdd2e620cc816089e8a22673e615ca6688bec17efcfa853bd789
d496c2
```

### Change, no memo

Recipient `m/35263'/1'/0'/1/0`, Receiving Address:

```
tf1lrkq0dfkh4q83pxp96rp62a4tlnn3ajw9gafvhd2uyg5vv3kmslegyut7k6ymyeygdpr5l4632djpd4a7vlfmt6pcrly3wj2rt2pxcgdelf8q
```

Inputs:

```
v     f71608bf3cb3a0dd14548b346ff0895205c884b77cc2759f530a3c08f06af005
r     0202020202020202020202020202020202020202020202020202020202020200
qty   99000000000
flv   0100000000000000000000000000000000000000000000000000000000000000
memo  "" (0 bytes)
```

Derived values:

```
S             f8ec07b536bd407884c12e861d2bb55fe738f64e2a3a965daae111463236dc3f
V             94138bf5b44d932443423a7eba8a9b20b6bdf33e9daf41c0fe48ba4a1ad41361
R             94f918d7c467161ccf16f49e03541bb01c9613c1d5d9661251e4b09dc0d4df6f
X             62cb64914c21ac6d6a5bb3f7aa50fb1200a3255d648cc09b1ca85a41b8cf705c
qty_blinding  5bfe4512ffa637e18ca856667d1a7252f95647c6b2d62b3cb6d6123d8359e20c
flv_blinding  11aab0c5db0f826c5aa4594e13c8698d66fc740bf86dac05e17834364f433f07
siv_key       970f556ca15017adda4240dc7ff7e63025ee5c8e1f5d9624e2563479cb97529a
C_qty         68b16eb4d12328cae5a907d543f6ff3618a30c4011a9503296d7416d1975f669
C_flv         8e9dde6f51f4c670565d76810e73c5179653c576e3aec7b5f1e418add1e8943c
tag           269e9e0cda8e0b63207ca6e4ef90b8d5
```

Note (89 bytes):

```
0194f918d7c467161ccf16f49e03541bb01c9613c1d5d9661251e4b09dc0d4df
6f269e9e0cda8e0b63207ca6e4ef90b8d58c3a010fde4de2174c7a96d403f52f
657f63b6cb0482814fd2e993170934740d2098c2b126cf72d0
```
