# flamepayments

The wallet side of Flame payments: keys, transfers, and the notes that
travel with them. The crate turns a flamekd seed into the addresses and keys
a wallet hands out. It builds, signs and packages a transfer, and seals an
encrypted note for every output it creates. It opens the note of an output
it receives, which gives the recipient everything it needs to spend that
output. It implements [Confidential payments](../docs/payments.md).

It holds nothing on disk and talks to no node. The caller fetches what the
crate opens.

## An account

An `Account<K>` is the node at `m/35263'/network'/0'`, held as one of
the three flamekd keys; the key is the type parameter, and what the
account can do follows from it:

| Type | Built by | Can |
| --- | --- | --- |
| `SpendAccount = Account<SpendKey>` | `from_seed` | find, open, spend |
| `ViewAccount = Account<ViewKey>` | `from_view_key`, `to_view_account()` | find, open |
| `ReceiveAccount = Account<RecvKey>` | `from_recv_key`, `to_receive_account()` | find |

Methods live where their key allows: `impl<K: AccountKey>` for addresses,
predicates, `owns` and the counter; `impl<K: ViewingKey>` for
`viewing_key_at` and `view_key`; `impl SpendAccount` for
`spending_key_at`. Calling past what the key allows does not compile.
Both traits are sealed to flamekd's keys. Code that needs only addresses
takes `&Account<K>` with `K: AccountKey`, and only notes `K: ViewingKey`.

`from_view_key` and `from_recv_key` must be given the account node's key,
as `view_key()` and `recv_key()` export it. The bech32f encoding carries no
depth, so nothing can check this: a key from any other node builds an
account that silently owns nothing.

Everything below the account node lives on the two normal branches of the
standard path — receiving (`0`) and change (`1`) — so an address is
`m/35263'/network'/0'/branch/n`. The path constants come from `flamekd::util`;
this crate never repeats their values.

Two derivations lead to the same place, and the difference matters:

- `address_at(branch, n)` goes through the account's **receiving key**, so
  the derivation itself touches no secret — it is the derivation an indexer
  holding only a receiving key would perform, and the one a
  `ReceiveAccount` does.
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
fee. An `OutputSpec` names a Receiving Address, a quantity, a flavor and a
memo; the builder derives the predicate, both blinding factors and the note
from them. The script it emits:

```text
per input:    push_str(String::contract(contract))  input  signtx
fee > 0:      push_int(fee)  fee                     one more mix input
per output:   push_str(commitment(qty))  push_str(commitment(flv))
              push_int(m)  push_int(n)  mix
per output i: roll_k(n-1-i) if > 0;  push_point(S_i)  output
              pushcell(note_i)  log
```

The outputs are sorted by their quantity commitment, compared as bytes,
before any of this is emitted, and the commitments, the predicates and the
notes all follow that one order. Under a fresh blinding factor the order
says nothing about which output is the change. A caller finds an output by
its predicate, never by its position.

`signtx` pushes the contract's single payload Value and no count, so with a
bare token payload there is nothing left on the stack to drop. `mix` balances
the inputs against the outputs per flavor and range-proves each output; the
fee, when there is one, is a negative debt that counts as one more input. A
zero fee emits no `fee` opcode at all.

The roll before each `output` is not decoration. `mix` leaves the output
tokens in the order their commitments were pushed and `output` pops from the
top, so without it every multi-output transfer would pair the first
predicate and note with the last amount. `pushcell(note) log` leaves the
stack as it found it, so the notes change no roll depth.

## Notes

Every output a transfer creates carries its note in the `Data` entry right
after it: the quantity, the flavor and the memo, encrypted to the address's
viewing key. [Confidential payments](../docs/payments.md) specifies the
derivations, the note layout and the receiving procedure. This crate
follows it and does not restate it.

- `build_transfer` seals the notes. It draws each output's one-time scalar
  `r` from the caller's generator through a Merlin transcript that binds the
  transfer's header, inputs, fee and outputs and is rekeyed with the inputs'
  signing keys. A weak generator then still gives different transfers
  different `r`. A memo longer than `MEMO_MAX`, 4006 bytes, is
  `BuilderError::MemoTooLong`.
- `outputs_with_notes(log)` pairs every output of a log with the entry
  right after it.
- `open_note(contract, note, address, v)` runs the receiving procedure. It
  returns a `ReceivedNote`: the `Opening` that `InputSpec::confidential`
  spends the output with, and the memo. Every other result is a
  `NoteError`.

No function of the `note` module returns the shared point `X`, the note key
or a blinding factor on its own. A blinding factor leaves the crate only
inside an `Opening`, which is what
[Disclosure](../docs/payments.md#disclosure) hands to a third party.

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

A lookahead policy: `owns` takes the gap as an argument, and how far past
its last used index a wallet looks is the wallet's choice. Talking to a
node. Anything on disk, and any command line. Issued tokens. Each belongs to
a later phase.
