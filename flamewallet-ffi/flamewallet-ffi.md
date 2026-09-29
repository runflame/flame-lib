# flamewallet-ffi

`flamewallet` behind a flat surface that UniFFI can carry to Swift and Kotlin,
and that a Node addon can wrap. The crate adds no wallet logic of its own: it
turns bytes into the types `flamewallet` takes, and the results back into
bytes.

It does no I/O. Contracts and membership proofs arrive as the bytes an indexer
serves — the encodings in `flamechain::codec` — and a signed transfer leaves as
`BlockTx::to_bytes`. Fetching the one and publishing the other is the app's
job.

## The boundary

Everything that crosses is bytes, strings, integers and plain records. Points
are 32 compressed bytes, scalars 32 canonical little-endian bytes, ids 32
bytes. The one exception is `Wallet`, an opaque handle around an `Account`: a
spending key is derived inside a call, used, and dropped before it returns, so
no binding ever holds one. Keys are named by path instead — `KeyPath { branch,
index }`, with branch `0` for receiving and `1` for change.

| Call | What it does |
| --- | --- |
| `generate_mnemonic(words)` | A fresh English BIP-39 phrase: 12, 15, 18, 21 or 24 words. |
| `validate_mnemonic(phrase)` | Whether the phrase is valid in any BIP-39 language. |
| `mnemonic_to_seed(phrase, passphrase)` | The 64-byte seed an app stores. |
| `Wallet::new(seed, network, next_index)` / `Wallet::from_mnemonic(..., next_index)` | The account `m/35263'/network'/0'`, with its receiving counter restored: the seed holds keys, not history. A new wallet passes 0. |
| `address(path)` / `next_address()` | bech32f address and its predicate; `next_address` advances the counter. |
| `next_index()` | The receiving counter, which an app stores after each `next_address` and passes back when it reopens the wallet. |
| `owns(predicate, gap)` | Which path, if any, a served contract belongs to. |
| `receiving_key()` | bech32f `recv…` key: what an indexer is given to find the wallet's payments. |
| `address_to_predicate(address, network)` | What an output to someone else is locked with. |
| `decode_contract(bytes)` | Id, predicate, and a cleartext amount if the contract has one. |
| `opening_matches(contract, opening)` | Whether an opening received out of band opens a contract. |
| `build_transfer(request)` | Build, prove and sign; returns txid, `BlockTx` bytes and each output's opening. |

Every error is a `FlameError`. `InvalidBytes { what }` names the field —
`contract`, `proof`, `predicate`, `flavor`, `blinding` — so an app can tell a
bad indexer response from a bad user input.

## A transfer

`TransferInput` is a contract as served, its proof at the tip being targeted,
the path of the key it is locked to, and, for a confidential contract, its
opening. Two checks run before any proving: the input's predicate must be the
one at its path (`KeyMismatch` otherwise — a wrong path would build, prove and
sign a transaction every node refuses), and the opening must rebuild the
published id.

Blinding factors for the outputs are drawn from the OS RNG inside the call.
`Transfer.outputs` returns, in request order, each new contract's id, bytes
and `Opening`. The opening is the only record an output can be spent from:
the recipient's must be delivered to them, and the change's kept. This crate
does not yet encrypt or deliver them; that is a later phase's note format.

The transaction version is fixed at 1, the only one a chain accepts. `gas` is
the caller's; a node refuses more than its block limit.

## Building

```sh
flamewallet-ffi/scripts/build-apple.sh     # target/flamewallet-ffi/apple/FlameWallet — a Swift package
flamewallet-ffi/scripts/build-android.sh   # target/flamewallet-ffi/android/{jniLibs,kotlin}
```

Both build with the `release-ffi` profile — `release` plus LTO, one codegen
unit and stripped symbols — and generate bindings from an unstripped host
build, since `uniffi-bindgen` reads the metadata the library embeds.

The Apple package is an xcframework (iOS device, simulator arm64 + x86_64,
macOS arm64 for previews and `swift test`) with the generated Swift as a
target over it. The static libraries are large on disk; the linker keeps only
what the app reaches.

Android needs the NDK and `cargo install cargo-ndk`. 64-bit libraries are
linked for 16 KB pages, which Google Play requires. The Kotlin loads
`libflamewallet_ffi.so` through JNA, so the app needs
`net.java.dev.jna:jna:<version>@aar`. The package is `com.flame.wallet`, set
in `uniffi.toml`.

`uniffi-bindgen` is a binary of this crate, behind the `cli` feature, so the
generator always matches the library's UniFFI version:

```sh
cargo run -p flamewallet-ffi --features cli --bin uniffi-bindgen -- generate --library <lib> --language swift --out-dir <dir>
```

## What is not here

A Node addon, which wraps this crate with napi-rs rather than UniFFI. Mnemonic
languages other than English for generation. Watch-only wallets, which wait on
`Account::from_recv_key`. Coin selection, note encryption, and anything on
disk.
