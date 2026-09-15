# Flame Key Derivation

FlameKD defines deterministic hierarchies with separate spending, viewing, and
receiving capabilities. It follows the public-child derivation approach of
[BIP 32](https://github.com/bitcoin/bips/blob/master/bip-0032.mediawiki).

The words MUST, MUST NOT, SHOULD, and MAY specify implementation requirements.

## Group and representations

All group operations use
[ristretto255](https://www.rfc-editor.org/rfc/rfc9496.html#section-4).
Its prime order is:

```
q = 2^252 + 27742317777372353535851937790883648493
```

Lowercase letters denote scalars modulo `q`; uppercase letters denote points.
Scalar addition and subtraction are modulo `q`. Point addition is the group
operation, and `x*G` denotes scalar multiplication by the standard generator.
The canonical compressed encoding of `G` is:

```
e2f2ae0a6abc4e71a884a961c500515f58e30b6aa582dd8db6a65945e08d2d76
```

`enc(x)` for a scalar is its canonical 32-byte little-endian representation,
whose unsigned integer value is less than `q`. Decoding MUST reject values
greater than or equal to `q`; it MUST NOT reduce imported scalars modulo `q`.
`enc(P)` for a point is its canonical 32-byte compressed Ristretto encoding.
Point decoding MUST use Ristretto decoding and reject invalid encodings.

Spending and viewing scalars `s` and `v` MUST be nonzero. Their public points
`S = s*G` and `V = v*G` MUST be nonidentity. Derivation material `t` is a scalar;
zero is permitted. No point `t*G` is used by this specification.

## Key types and capabilities

The tuple order below is normative, including for binary serialization.

| Type | Tuple | Capability |
| --- | --- | --- |
| `SpendKey` | `(s, v, t)` | Spend, view, and generate addresses; derive normal and hardened children. |
| `ViewKey` | `(S, v, t)` | View contents and generate addresses; derive normal children. |
| `RecvKey` | `(S, V, t)` | Generate receiving addresses and track their activity; derive normal children. |
| `ReceivingAddress` | `(S, V)` | Receive encrypted funds at this address. |
| `TrackingAddress` | `(S)` | Receive clear funds and identify activity at this spending public key. |

`RecvKey` cannot decrypt encrypted contents or authorize spending. A
`TrackingAddress` does not identify other addresses in the account. Neither
address type can derive children, because neither contains `t`.

Scalar `s` authorizes spending at its own `S`; scalar `v` enables viewing at its
own `V`. Hierarchical access requires the corresponding extended key. Derivation
material `t` alone is not a tracking capability.

`ViewKey` and `RecvKey` cover their own node and descendants reached using only
normal edges. They cannot cross a hardened edge, even when exported at the root.
To share access to a hardened account, derive that account using `SpendKey` and
then export its `ViewKey` or `RecvKey`. This specification does not prescribe
account paths or address-discovery lookahead rules.

## Conversions

Conversions are deterministic and do not modify the source key.

| Conversion | Result |
| --- | --- |
| `SpendKey(s, v, t)` to `ViewKey` | `(s*G, v, t)` |
| `ViewKey(S, v, t)` to `RecvKey` | `(S, v*G, t)` |
| `RecvKey(S, V, t)` to `ReceivingAddress` | `(S, V)` |
| `ReceivingAddress(S, V)` to `TrackingAddress` | `(S)` |

Conversions MAY be composed. For example, `SpendKey` can produce every public
address type. The reverse conversions are not provided: removing secrets or
derivation material does not make them recoverable from the resulting value.

## Seed input

The root-derivation input is exactly 64 bytes. An already available binary seed
MUST be consumed directly, without interpreting it as text or hashing it first.
Other binary lengths MUST be rejected.

Mnemonic input MUST first be converted to the standard
[BIP 39 seed](https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki#from-mnemonic-to-seed).
Use the mnemonic sentence and passphrase normalized with Unicode NFKD and
encoded as UTF-8. An omitted passphrase is the empty string. The conversion is:

```
seed = PBKDF2-HMAC-SHA512(
    password = UTF8(NFKD(mnemonic)),
    salt = UTF8("mnemonic" + NFKD(passphrase)),
    iterations = 2048,
    output_length = 64
)
```

Mnemonic import MUST validate the BIP 39 wordlist, length, and checksum, then
serialize the mnemonic words separated by single ASCII spaces before the seed
conversion. Passphrases MUST NOT be trimmed or case-folded. Implementations MUST
NOT use mnemonic entropy bytes in place of the 64-byte BIP 39 seed. The same
mnemonic words and passphrase produce the same 64-byte seed supplied to BIP 32.
FlameKD feeds those bytes directly into its own root transcript; it does not
apply BIP 32's master-key HMAC with key `"Bitcoin seed"`.

## Transcript conventions

`Transcript(domain)` creates a fresh
[Merlin transcript](https://merlin.cool/) using the domain as its initialization
label. Every label in this specification is an exact, case-sensitive ASCII byte
string, without a terminating zero byte.

`P.append(label, bytes)` is Merlin's `append_message` operation. Scalar and point
messages use `enc`; indices use `LE32`, an unsigned four-byte little-endian
encoding. No extra length, type, or tuple encoding is added to these messages;
Merlin supplies its own framing.

`P.challenge_scalar(label)` requests exactly 64 challenge bytes from Merlin,
interprets them as one unsigned 512-bit little-endian integer, and reduces that
integer modulo `q`. It is not a 32-byte challenge or rejection sampling.

All appends and challenges occur sequentially in the stated order on the same
transcript. Each root or child derivation starts a fresh transcript. Transcript
state is not reused across derivations or cloned separately for each challenge.

## Root derivation

Given the 64-byte seed:

```
P = Transcript("FlameKD.from_seed")
P.append("seed", seed)
s = P.challenge_scalar("s")
v = P.challenge_scalar("v")
t = P.challenge_scalar("t")
return SpendKey(s, v, t)
```

If `s = 0` or `v = 0`, root derivation MUST return an error. It MUST NOT retry
with an altered seed, add a counter, or silently choose another transcript.

## Child indices

An index `i` is an unsigned 32-bit integer, `0 <= i < 2^32`.

* `0 <= i < 2^31` selects normal derivation.
* `2^31 <= i < 2^32` selects hardened derivation.

The entire value, including the hardened bit, is appended as `LE32(i)`.
Indices outside the unsigned 32-bit range MUST be rejected. A `ViewKey` or
`RecvKey` MUST reject hardened indices before attempting child derivation.

## Normal derivation

Every normal derivation uses the same parent public values `(S, V, t)`.
`SpendKey` computes `S = s*G` and `V = v*G`; `ViewKey` computes `V = v*G`;
`RecvKey` already contains both points.

For a normal index `i`, generate the child adjustments and derivation material:

```
P = Transcript("FlameKD.derivation")
P.append("S", enc(S))
P.append("V", enc(V))
P.append("t", enc(t))
P.append("i", LE32(i))
ds = P.challenge_scalar("ds")
dv = P.challenge_scalar("dv")
t_child = P.challenge_scalar("t")
```

Return the corresponding child type:

```
SpendKey(s, v, t) -> SpendKey(s + ds, v + dv, t_child)
ViewKey(S, v, t)  -> ViewKey(S + ds*G, v + dv, t_child)
RecvKey(S, V, t)  -> RecvKey(S + ds*G, V + dv*G, t_child)
```

`ds`, `dv`, and `t_child` may be zero. The resulting spending and viewing keys
must still be valid: `S + ds*G` and `V + dv*G` MUST both be nonidentity.
Equivalently, any available child secret scalar MUST be nonzero.
If either result is invalid, derivation MUST return an error for that index.
It MUST NOT retry, increment the index, or alter the transcript.

For every successful normal derivation, conversion commutes with derivation:

```
view(derive(spend, i)) = derive(view(spend), i)
recv(derive(view, i))  = derive(recv(view), i)
```

Converting these equal children to either address type also gives equal results.
All three parent types can detect either invalid child public point and MUST
agree on whether a normal derivation succeeds.

## Hardened derivation

Only `SpendKey(s, v, t)` can derive a hardened child. Both secret scalars are
committed, including the secret `v`, not its public point `V`:

```
P = Transcript("FlameKD.hardened")
P.append("s", enc(s))
P.append("v", enc(v))
P.append("t", enc(t))
P.append("i", LE32(i))
ds = P.challenge_scalar("ds")
dv = P.challenge_scalar("dv")
t_child = P.challenge_scalar("t")
return SpendKey(s + ds, v + dv, t_child)
```

If either child secret is zero, derivation MUST return an error for that index,
without retry or index increment. A successful hardened child can subsequently
be converted to its `ViewKey`, `RecvKey`, and addresses.

## Bech32f encoding

The `f` in Bech32f stands for Flame. Bech32f uses the alphabet, six-symbol
checksum, HRP expansion, and polymod algorithm of
[Bech32m](https://github.com/bitcoin/bips/blob/master/bip-0350.mediawiki#bech32m),
including the constant `0x2bc830a3`. Original Bech32 checksums with constant `1`
MUST NOT be accepted.

The alphabet, in value order from 0 through 31, is:

```
qpzry9x8gf2tvdw0s3jn54khce6mua7l
```

Unlike BIP 173/350, Bech32f replaces the 90-character maximum with the exact
type-dependent lengths below. This length override has precedent in
[ZIP 316](https://zips.z.cash/zip-0316).
The payload is the raw tuple below, and the HRP identifies its type.

| Type | HRP | Raw tuple | Bytes | Payload symbols | Total characters |
| --- | --- | --- | --- | --- | --- |
| `SpendKey` | `spend` | `enc(s) + enc(v) + enc(t)` | 96 | 154 | 166 |
| `ViewKey` | `view` | `enc(S) + enc(v) + enc(t)` | 96 | 154 | 165 |
| `RecvKey` | `recv` | `enc(S) + enc(V) + enc(t)` | 96 | 154 | 165 |
| `ReceivingAddress` | `f` | `enc(S) + enc(V)` | 64 | 103 | 111 |
| `TrackingAddress` | `c` | `enc(S)` | 32 | 52 | 60 |

`+` is byte concatenation. Encode the complete raw tuple in one 8-to-5-bit
conversion, reading each byte most-significant bit first. Pad only the final
group with zero bits: two bits for 96 bytes, three for 64 bytes, four for 32 bytes.
Do not independently pad the 32-byte elements.

The text is `HRP || "1" || payload || checksum`. Compute the checksum using the
lowercase HRP and converted payload symbols. The six checksum symbols are the
30-bit result of the Bech32m checksum construction, most-significant group first.
Verification MUST yield polymod `0x2bc830a3` over the expanded HRP and all symbols.

Encoders MUST emit lowercase ASCII. Decoders MUST accept all-lowercase and
all-uppercase encodings, reject mixed case, and normalize accepted uppercase
input to lowercase before interpreting the HRP and verifying the checksum.
Decoders MUST reject unknown HRPs, unexpected types, incorrect lengths, invalid
alphabet symbols, whitespace, non-ASCII text, and checksum failures.

After removing the checksum, decode 5-to-8 bits without adding padding. Any
remaining bits MUST be fewer than five and all zero. The decoded length MUST
match the HRP's exact byte length. Split the raw tuple only after this check,
then validate every scalar and point as specified above, including nonzero
`s`/`v` and nonidentity `S`/`V`. No trailing bytes or fields are permitted.

### Error detection

For a fixed valid HRP and fixed length, the checksum detects every pattern of
up to three substituted payload/checksum symbols at all lengths used here.
It also detects up to four substitutions when they fit within an 89-symbol
window. There is no guarantee of detecting every four-symbol substitution
across the longer encodings. These statements concern changed five-bit symbols,
not case normalization, HRP changes, insertions, or deletions. See the
[BIP 173 checksum properties](https://github.com/bitcoin/bips/blob/master/bip-0173.mediawiki#checksum-design)
and [BIP 350 analysis](https://github.com/bitcoin/bips/blob/master/bip-0350.mediawiki#appendix-checksum-design--properties).
The three-substitution guarantee extends to 1023 symbols, as described in
[Bitcoin Core's polymod implementation](https://github.com/bitcoin/bitcoin/blob/master/src/bech32.cpp).

For fixed HRP and payload, a uniformly random six-symbol checksum is accepted
with probability `2^-30`. This is not a bound for every possible error pattern.
The checksum is not authentication; anyone can recompute it for modified data.

These bounds guarantee unique correction of one substituted payload/checksum
symbol when the HRP is fixed. They do not guarantee unique correction of two
substitutions across the longer strings.

Decoders MUST NOT automatically correct an invalid encoding. A user interface
MAY suggest possible error positions, but MUST NOT claim that the number of
errors is known or silently replace the supplied value with a guessed key.

## Compromise boundaries

All extended keys contain account-linking material and SHOULD be shared only
with parties authorized for their stated capabilities. Neither address format
encodes its derivation index or `t`. This does not promise anonymity against
transaction metadata or other external information.

A parent `RecvKey` can calculate normal-child `ds` and `dv`. Consequently:

- A disclosed normal child viewing scalar reveals parent `v = v_child - dv`.
- A disclosed normal child spending scalar reveals parent `s = s_child - ds`.
- Both disclosures, even from different normal descendants, reveal the parent
  `SpendKey`. A parent `ViewKey` plus a child spending scalar also suffices.

The same equations apply along a known entirely normal path by accumulating
adjustments. Hardened derivation requires both parent secret scalars: recovering
only `s`, while knowing `S`, `V`, and `t`, does not enable hardened derivation.
Use hardened account boundaries when independently delegated subtrees must not
expose their ancestors through these normal-child disclosure relationships.

Token encryption is outside this specification. An encryption protocol SHOULD
use fresh sender randomness per output, validated group inputs, and a
domain-separated KDF bound to the output and recipient; see
[HPKE](https://www.rfc-editor.org/rfc/rfc9180.html#section-4.1).
For selective disclosure, share the final key for the selected encrypted output,
not a viewing scalar or the raw DH point. Raw DH disclosure can also unlock
other outputs when sender points repeat or have known scalar relationships.

## Test vectors

These vectors use a binary seed of 64 zero bytes. This is public test material,
not a seed to use for funds. Hex values below are split at 32-byte boundaries;
concatenate the lines without whitespace. Paths are descriptive only: `m/0'`
uses index `0x80000000`, and `m/0'/16909060` then uses normal index `0x01020304`
(encoded `04 03 02 01`).

### m

`SpendKey` bytes (`s || v || t`):

```
4d7864baf60fae7dfc13f6ae21ca3d952effb8150d31f676aabb8385dab5ff07
7436bce57cafb4e2ac373fb1313d7a3d8ba3cc9dd7d37e4ddf4ed80c73ff6f07
56174cc1d26976a5d1a307b2fb4197379a2f8ba40112452f9f7bc92b2d01f104
```

`RecvKey` bytes (`S || V || t`):

```
58a8ba7ce9e780c32ba28ecdc159508e8afccb74596c6f29f94eff0552ba4336
667506790c9021acfc302d3e6f1d0f65854a061cf87b44b51d3c885dde27da42
56174cc1d26976a5d1a307b2fb4197379a2f8ba40112452f9f7bc92b2d01f104
```

Receiving address:

```
f1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvmxvagx0yxfqgdvlscz60n0r58ktp22qcw0s76yk5wnezzamcna5ssfgcs3c
```

### m/0

`SpendKey` bytes (`s || v || t`):

```
ea4b455c0ca4612a80764577662c106e2ddda504bfe8dbbb8046bb9207eb2402
e5a06f069a42ec117326f2f35b72e70cd7502bbff1076f7c707bf93b75edd600
89cc4fc8023b48936cd3af9259d4732ac42d373064040470ff0b8155003a5b0c
```

`RecvKey` bytes (`S || V || t`):

```
6c9d43624413b329de20094274dbeebbf43535f7ff307242c7fdd5248a088646
8640e89f0cb97677ac949a3cd53710c4fd82c5bcfb16d4f45bcbc010a4884a31
89cc4fc8023b48936cd3af9259d4732ac42d373064040470ff0b8155003a5b0c
```

Receiving address:

```
f1djw5xcjyzwejnh3qp9p8fklwh06r2d0hluc8ysk8lh2jfzsgsergvs8gnuxtjanh4j2f50x4xugvflvzck70k9k573duhsqs5jyy5vgdmtj30
```

### m/0'

`SpendKey` bytes (`s || v || t`):

```
d0ce5cf6dbcf65e686f93acd06ea7b3a2c3ba85c4801edfe909b9aaea0f1950d
b1f4e8583a3444c70f2331b85562851e50ff564ea3d962ebb717b64bb5408208
6b1b87cbad0acf6a556ef6e8a93e9f5d04e875ba3daadaf3f6373b9295e02502
```

`RecvKey` bytes (`S || V || t`):

```
8afd6a2bb1c7bacf2b3e50255a829fcb1838d78d4d727b84fa3d16828fab702e
3496bb96aaab48254a706f7e8af47860c4ad49f73fbeb602d92b22bffd9cc472
6b1b87cbad0acf6a556ef6e8a93e9f5d04e875ba3daadaf3f6373b9295e02502
```

Receiving address:

```
f13t7k52a3c7av72e72qj44q5levvr34udf4e8hp8685tg9ratwqhrf94mj642kjp9ffcx7l5273uxp39df8mnl04kqtvjkg4llkwvgushdv07q
```

### m/0'/16909060

`SpendKey` bytes (`s || v || t`):

```
17df83b0eb14baecdbeec3a28aae14f160c8e5f325811bb268182e63293a0004
33e008085c544e574b46edc30444b616f9e4935324e1bb2aa4146a84fdf3f70f
2937614f4eab320d2c9af3c24c87c32fb1c508c85189df879a030259469eda09
```

`RecvKey` bytes (`S || V || t`):

```
8a519096ebca5e78f98204887dd2a914e320d37d628ed9f4b49b21a469c0ef14
b2aac2520ba5a2d4ee78796b9f8b0c563df10c96af7b26979268d5a38560ab06
2937614f4eab320d2c9af3c24c87c32fb1c508c85189df879a030259469eda09
```

Receiving address:

```
f13fgep9htef0837vzqjy8m54fzn3jp5mav28dna95nvs6g6wqau2t92kz2g96tgk5aeu8j6ul3vx9v003pjt277exj7fx34drs4s2kps2e0jrr
```

### Root extended keys and tracking address

For the same root, the other four Bech32f encodings are:

`SpendKey`:

```
spend1f4uxfwhkp7h8mlqn76hzrj3aj5h0lwq4p5clva42hwpctk44lurhgd4uu472ld8z4smnlvf384armzarejwa05m7fh05akqvw0lk7p6kzaxvr5nfw6jargc8kta5r9ehnghchfqpzfzjl8mmey4j6q03qs4yv900
```

`ViewKey`:

```
view1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvm8gd4uu472ld8z4smnlvf384armzarejwa05m7fh05akqvw0lk7p6kzaxvr5nfw6jargc8kta5r9ehnghchfqpzfzjl8mmey4j6q03qszcekyq
```

`RecvKey`:

```
recv1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvmxvagx0yxfqgdvlscz60n0r58ktp22qcw0s76yk5wnezzamcna5sjkzaxvr5nfw6jargc8kta5r9ehnghchfqpzfzjl8mmey4j6q03qse6tpf4
```

`TrackingAddress`:

```
c1tz5t5l8fu7qvx2az3mxuzk2s3690ejm5t9kx720efmls2546gvmq72w85x
```

### BIP 39 compatibility

The English mnemonic `abandon` repeated eleven times followed by `about`, with
passphrase `TREZOR`, MUST produce the following 64-byte intermediate seed:

```
c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6
987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04
```

Concatenate these hex lines. This is the BIP 39 reference vector, not the
all-zero seed used for the FlameKD vectors above.
