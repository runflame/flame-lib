# flamewallet-wasm

`flamewallet-ffi` for JavaScript (browser, Electron, Node), published as
`@runflame/wallet-wasm`. Same calls in camelCase; bytes are `Uint8Array`,
amounts `bigint`, errors a thrown `FlameError` with `kind`. Types are in
`src/types.d.ts`.

`buildTransfer` is synchronous (~80 ms) — run it in a Web Worker.

## Building

```sh
flamewallet-wasm/scripts/build.sh    # target/wallet-wasm/pkg
node --test flamewallet-wasm/test/*.test.mjs
```

Needs `wasm-bindgen` at the version in `Cargo.lock`. Regenerate test
fixtures after an encoding change:

```sh
FLAME_FIXTURES_OUT=$PWD/flamewallet-wasm/test/fixtures.json \
  cargo test -p flamewallet-wasm --test fixtures -- --ignored
```

Releasing: see `flamewallet-ffi/flamewallet-ffi.md`.

## Loading

```ts
import init, { Wallet } from '@runflame/wallet-wasm';
await init();
```

In Node, pass the bytes to `initSync({ module })`. A CSP needs
`'wasm-unsafe-eval'` in `script-src`.
