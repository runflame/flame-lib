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
| `Wallet::new(seed, network, next_index)` / `Wallet::from_mnemonic(...)` | Spend wallet, account `m/35263'/network'/0'`. |
| `Wallet::from_view_key(view_key, network, next_index)` | View wallet: finds and opens, cannot spend. |
| `Wallet::from_receiving_key(receiving_key, network, next_index)` | Receive wallet: finds, cannot open or spend. |
| `kind()` | `Spend`, `View` or `Receive`. |
| `address(path)` / `next_address()` | bech32f address and predicate. |
| `next_index()` | Receiving counter to persist. |
| `owns(predicate, gap)` | Path a contract belongs to, if any. |
| `receiving_key()` | `recv…` key a receive wallet is built from. |
| `view_key()` | `view…` key a view wallet is built from; not on a receive wallet. |
| `address_to_predicate(address, network)` | Predicate an address's payments are locked with. |
| `decode_contract(bytes)` | Id, predicate, cleartext amount. |
| `open_note(contract, note, path)` | Opening and memo from an output's note; not on a receive wallet. |
| `opening_matches(contract, opening)` | Whether an opening opens a contract. |
| `build_transfer(request)` | Txid, `BlockTx` bytes, each output with its note; spend wallet only. |

Errors are `FlameError`; `InvalidBytes { what }` names the bad field,
`Note { failure }` says why a note did not open, and `NotPermitted { wallet,
needs }` says a call needs a wallet built from a stronger key.

Outputs are paid to bech32f addresses. Each carries an encrypted note, and
only its recipient can open it: amount, blinding factors and memo, as
`docs/payments.md` specifies. A scan serves each contract with its note:
`owns` finds the path, `open_note` gives the opening, and a later
`TransferInput` spends with it. Change is opened the same way. Created
outputs come back in published order, which is sorted, not request order.

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
