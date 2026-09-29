# flamewallet-ffi

UniFFI bindings of `flamewallet` for Swift and Kotlin. No I/O: contracts and
proofs come in as `flamechain::codec` bytes, a signed transfer leaves as
`BlockTx::to_bytes`.

Everything that crosses is bytes, strings, integers and records. `Wallet` is
an opaque handle; spending keys never leave a call. Keys are named by
`KeyPath { branch, index }` — branch `0` receiving, `1` change.

| Call | What it does |
| --- | --- |
| `generate_mnemonic(words)` | English BIP-39 phrase, 12–24 words. |
| `validate_mnemonic(phrase)` | Whether the phrase is valid. |
| `mnemonic_to_seed(phrase, passphrase)` | 64-byte seed. |
| `Wallet::new(seed, network, next_index)` / `Wallet::from_mnemonic(...)` | Account `m/35263'/network'/0'`. |
| `address(path)` / `next_address()` | bech32f address and predicate. |
| `next_index()` | Receiving counter to persist. |
| `owns(predicate, gap)` | Path a contract belongs to, if any. |
| `receiving_key()` | `recv…` key for an indexer. |
| `address_to_predicate(address, network)` | Predicate for an output. |
| `decode_contract(bytes)` | Id, predicate, cleartext amount. |
| `opening_matches(contract, opening)` | Whether an opening opens a contract. |
| `build_transfer(request)` | Txid, `BlockTx` bytes, output openings. |

Errors are `FlameError`; `InvalidBytes { what }` names the bad field.

Output openings are the only way to spend an output: deliver the recipient's,
keep the change's.

## Building

```sh
flamewallet-ffi/scripts/build-apple.sh     # target/flamewallet-ffi/apple/FlameWallet
flamewallet-ffi/scripts/build-android.sh   # target/flamewallet-ffi/android/{jniLibs,kotlin}
flamewallet-ffi/scripts/package-native.sh  # target/wallet-rn/pkg — @runflame/wallet-rn
cd flamewallet-ffi/host-tests/swift && swift test
```

Extra rustflags go in `FLAME_RUSTFLAGS`, not `RUSTFLAGS`. Android needs the
NDK and `cargo-ndk`; the app needs `net.java.dev.jna:jna:<version>@aar`.

## Releasing

`.github/workflows/release-wallet.yml` publishes `@runflame/wallet-rn` and
`@runflame/wallet-wasm`, each at its crate's version:

- **push to `main`** — `X.Y.Z-rc.N` under `next`;
- **tag `flamewallet-vX.Y.Z`** — both packages, `latest` and a GitHub release
  (`-rc.N` tags go to `next`). Both `Cargo.toml`s must be at `X.Y.Z`.

Publishing uses npm trusted publishing (no token). A package's first version
is published by hand, then its trusted publisher is set on npmjs.com:
GitHub Actions, `runflame/flame-lib`, `release-wallet.yml`, environment `npm`.
