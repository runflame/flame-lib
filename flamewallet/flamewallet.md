# flamewallet

Keys and transfers for a Flame wallet. The crate does two things: it turns a
flamekd seed into the addresses and keys a wallet hands out, and it builds,
signs and packages a transfer. It holds nothing on disk, encrypts nothing, and
knows nothing about scanning the chain.

## An account

An `Account` is the node at `m/35263'/network'/0'`, derived from a 64-byte
seed. Everything below it lives on the two normal branches of the standard
path — receiving (`0`) and change (`1`) — so an address is
`m/35263'/network'/0'/branch/n`. The path constants come from `flamekd::util`;
this crate never repeats their values.

Two derivations lead to the same place, and the difference matters:

- `address_at(branch, n)` goes through the account's **receiving key**, so
  the derivation itself touches no secret — it is the derivation an indexer
  holding only a receiving key would perform. Note that an `Account` cannot
  yet be built that way: `from_seed` is the only constructor, so a watch-only
  account waits on a `from_recv_key` that does not exist today.
- `spending_key_at(branch, n)` goes through the spending key and returns the
  scalar by value. The extended key it came from zeroizes on drop and never
  leaves the module; the scalar it hands back does not, so protecting that
  copy is the caller's job.

`owns(point, gap)` answers the only question a wallet asks of a stranger's
contract: is this mine? It re-derives every address below `next_index + gap`
on both branches, receiving first, and returns the `(branch, n)` that matches.
The gap covers addresses issued by another copy of the same wallet.

## An address and a predicate

An address is flamekd's `ReceivingAddress`, the pair `(S_n, V_n)`, rendered as
bech32f — `tf1…` on testnet, `f1…` on mainnet.

A **predicate** is what actually locks a contract on chain, and here it is
`Predicate::opaque(S_n.compress())`: the bare public spending key, with no
Taproot tree behind it. A spend authorizes by signing the transaction with
`signtx`, which records the predicate's point and the contract id for a MuSig
aggregate over the whole transaction. Script branches are a later phase's
problem; this crate needs none.

## A transfer

A transfer spends `InputSpec`s and creates `OutputSpec`s, with an optional
fee. The script it emits:

```text
per input:    push_str(String::contract(contract))  input  signtx
fee > 0:      push_int(fee)  fee                     one more mix input
per output:   push_str(commitment(qty))  push_str(commitment(flv))
              push_int(m)  push_int(n)  mix
per output i: roll_k(n-1-i) if > 0;  push_point(predicate)  output
```

`signtx` pushes the contract's single payload Value and no count, so with a
bare token payload there is nothing left on the stack to drop. `mix` balances
the inputs against the outputs per flavor and range-proves each output; the
fee, when there is one, is a negative debt that counts as one more input. A
zero fee emits no `fee` opcode at all.

The roll before each `output` is not decoration. `mix` leaves the output
tokens in spec order and `output` pops from the top, so without it every
multi-output transfer would pair the first recipient with the last amount.

## Why inputs travel as witnesses

**A spend must publish nothing about the amount it spends.** That is the whole
point of the transfer builder.

A confidential output on chain is a contract holding a `Token`: two Pedersen
commitments, `qty` and `flv`, stored as points. To spend it, the prover has to
convince the constraint system it knows what those points hide. The VM offers
two ways, and only one of them keeps the secret.

The wrong way is the `decrypt` opcode. It pops the quantity and both blinding
factors off the stack, which means they were pushed — as script literals, in
the bytecode. The verifier re-executes the same bytes. Every amount spent this
way is public on chain one hop after it was received, and the confidentiality
the commitments bought is spent along with it. **This crate emits no
`decrypt`.**

The right way is the witness path. `String::contract(c)` takes a whole
contract whose token halves are `Commitment::Open` — carrying the prover's
value and blinding factor — and pushes it. The emitted bytecode contains only
the contract's 32-byte id. `build_tx` collects the contract's *public* body
into the transaction's execution bag, where the token is again just two
points. On `input`, the VM resolves that public body and, on the prover side
alone, restores the private openings. The openings never reach the bag, the
bytecode, or the wire.

`InputSpec::confidential` is the only way to build such an input, and it is
where the opening is checked. It rebuilds the contract with open commitments
and compares the result's id to the published one. A Contract cell encodes the
commitment *points* only, so the rebuilt contract has the same id exactly when
the opening is the right one. The check happens before any script exists: a
wrong opening is a `BuilderError`, not a proving failure.

This is also why `InputSpec::clear` refuses a `Token` payload. A published
token's commitments are closed; handed to the prover it would reach `mix`
with nothing to prove and fail there, deep inside the VM. The constructors
exist to turn that into an error at the call site.

The gap this closed upstream was `flamevm::Token::from_opening`. `Token::new`
is crate-private and `Token::cleartext` builds only unblinded commitments, so
before it, no code outside `flamevm` could construct a token it was able to
open — and the witness path was unreachable from a wallet.

## The sixteen-output bound

`MAX_OUTPUTS` is 16, and the number is not a policy choice. `ScriptBuilder`'s
`roll_k` encodes `k` in the opcode's low nibble, so `k` above 15 would silently
wrap: the prover would prove one program and publish another. The first of `n`
outputs is rolled from depth `n - 1`, which makes 16 the exact limit. A
seventeenth output is `BuilderError::TooManyOutputs`.

Sixteen is the limit of the *encoding*, not of what can be proven. `mix`
range-proves each output over 64 bits, and the prover's shared
`BulletproofGens::new(1024, 1)` runs out of multipliers first: a transfer
with one input proves at thirteen outputs and fails at fourteen with
`VMError::R1CSProofConstruction`, an error that names nothing useful. More
inputs lower the ceiling further. So `MAX_OUTPUTS` is a guard against a
silent `roll_k` overflow rather than a limit anyone reaches, and a wallet
splitting into many outputs should expect the proving ceiling first.

## What is not here

Notes and their encryption, which [Confidential payments](../docs/payments.md)
specifies. Address discovery and chain scanning. Anything on disk, and any
command line. Issued tokens. Each belongs to a later phase.
