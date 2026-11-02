// The browser wallet: `@runflame/wallet-wasm` from npm. Vite serves its `.wasm` as a build asset
// and the module fetches it from that URL, so the service needs no wallet files of its own.
import init, { generateMnemonic, Wallet } from '@runflame/wallet-wasm';
import wasmUrl from '@runflame/wallet-wasm/flamewallet_wasm_bg.wasm?url';

let loading = null;

/** Instantiates the module once; every caller awaits the same instance. */
export default function load() {
  loading ??= init({ module_or_path: wasmUrl });
  return loading;
}

export { generateMnemonic, Wallet };
