/** Which network the address's two bech32f components are encoded for. */
export type Network = "mainnet" | "testnet";

/**
 * What a `Wallet` was built from, and so what it can do: `spend` (seed or
 * mnemonic) finds, opens and spends; `view` (view key) finds and opens;
 * `receive` (receiving key) only finds.
 */
export type WalletKind = "spend" | "view" | "receive";

/** `m/35263'/network'/0'/branch/index`; branch 0 is receiving, 1 is change. */
export interface KeyPath {
  branch: number;
  index: number;
}

export interface IssuedAddress {
  path: KeyPath;
  /** What a payer is given: the tracking address followed by the encryption key, `f1…e1…` / `t1…i1…`. */
  receivingAddress: string;
  /** The receiving address's first half, `f1…` / `t1…`, as `predicateToTrackingAddress` gives it. */
  trackingAddress: string;
  /** The 32-byte compressed point payments to this address are locked with. */
  predicate: Uint8Array;
}

export interface ContractInfo {
  /** The 32-byte contract id. */
  id: Uint8Array;
  /** What `Wallet.owns` is asked about. */
  predicate: Uint8Array;
  value: ContractValue;
}

export type ContractValue =
  | {
      kind: "clear";
      qty: bigint;
      flavor: Uint8Array;
      /** Whether `flavor` is `nativeFlavor()`, flames. */
      isNative: boolean;
    }
  | { kind: "confidential" }
  | { kind: "other" };

/** What a confidential output's recipient needs to spend it. Scalars are 32 bytes. */
export interface Opening {
  qty: bigint;
  flavor: Uint8Array;
  qtyBlinding: Uint8Array;
  flavorBlinding: Uint8Array;
}

export interface TransferInput {
  /** The contract's published bytes, as the indexer served them. */
  contract: Uint8Array;
  /** Its Utreexo membership proof at the tip the transfer targets; `replaceProofs` refreshes it after the tip moves. */
  proof: Uint8Array;
  /** The path of the key the contract is locked to. */
  path: KeyPath;
  /** Required for a confidential contract, refused for a cleartext one. */
  opening?: Opening;
}

export interface TransferOutput {
  /** The recipient's bech32f address, for the wallet's network. */
  address: string;
  qty: bigint;
  /** Absent for flames. */
  flavor?: Uint8Array;
  /** Sealed into the note for the recipient alone; at most `memoMaxBytes()`. */
  memo?: Uint8Array;
  /** Publish a cleartext token: amount and flavor public, no note, `memo` in the clear. */
  clear?: boolean;
}

export interface TransferRequest {
  inputs: TransferInput[];
  outputs: TransferOutput[];
  /** In sparks. Inputs equal outputs plus fee, per flavor. */
  fee: bigint;
  gas: bigint;
  /** 0 or absent for none. */
  locktime?: number;
}

export interface CreatedOutput {
  contractId: Uint8Array;
  /** Its published bytes, as an indexer serves them once confirmed. */
  contract: Uint8Array;
  /** The encrypted note that follows it on chain; `Wallet.openNote` reads it. */
  note: Uint8Array;
}

export interface Transfer {
  /** The 32-byte transaction id. */
  txid: Uint8Array;
  /** `BlockTx::to_bytes`: what a node's `submit_tx` takes. */
  blockTx: Uint8Array;
  /** In published order, not request order: match each by its predicate. */
  outputs: CreatedOutput[];
}

/** What `Wallet.openNote` gives: the opening to spend with, and the memo. */
export interface ReceivedNote {
  opening: Opening;
  /** Whether `opening.flavor` is `nativeFlavor()`, flames. */
  isNative: boolean;
  memo: Uint8Array;
}

/** Why a note did not open; `docs/payments.md` "Receiving" says what to do. */
export type NoteFailure =
  | "missing"
  | "malformed"
  | "unknownVersion"
  | "undecryptable"
  | "openingMismatch"
  | "notConfidential";

/** What every call throws. Fields beyond `kind` depend on it. */
export interface FlameError extends Error {
  name: "FlameError";
  kind:
    | "invalidMnemonic"
    | "invalidSeed"
    | "invalidKey"
    | "notPermitted"
    | "invalidAddress"
    | "invalidKeyPath"
    | "invalidBytes"
    | "keyMismatch"
    | "note"
    | "transfer";
  reason?: string;
  /** For `invalidBytes`: contract, proof, proofs, block_tx, predicate, flavor or blinding. */
  what?: string;
  /** For `keyMismatch`: the input's position. */
  input?: number;
  /** For `note`: how it failed. */
  failure?: NoteFailure;
  /** For `notPermitted`: the wallet that refused. */
  wallet?: WalletKind;
  /** For `notPermitted`: the least kind of wallet that could. */
  needs?: WalletKind;
}
