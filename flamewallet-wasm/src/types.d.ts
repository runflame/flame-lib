/** Which network addresses are encoded for: `f1…` or `tf1…`. */
export type Network = "mainnet" | "testnet";

/** `m/35263'/network'/0'/branch/index`; branch 0 is receiving, 1 is change. */
export interface KeyPath {
  branch: number;
  index: number;
}

export interface IssuedAddress {
  path: KeyPath;
  /** bech32f: `f1…` on mainnet, `tf1…` on testnet. */
  address: string;
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
  | { kind: "clear"; qty: bigint; flavor: Uint8Array }
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
  /** Its Utreexo membership proof at the tip the transfer targets. */
  proof: Uint8Array;
  /** The path of the key the contract is locked to. */
  path: KeyPath;
  /** Required for a confidential contract, refused for a cleartext one. */
  opening?: Opening;
}

export interface TransferOutput {
  predicate: Uint8Array;
  qty: bigint;
  /** Absent for flames. */
  flavor?: Uint8Array;
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
  /** The only record the output can be spent from. */
  opening: Opening;
}

export interface Transfer {
  /** The 32-byte transaction id. */
  txid: Uint8Array;
  /** `BlockTx::to_bytes`: what a node's `submit_tx` takes. */
  blockTx: Uint8Array;
  outputs: CreatedOutput[];
}

/** What every call throws. Fields beyond `kind` depend on it. */
export interface FlameError extends Error {
  name: "FlameError";
  kind:
    | "invalidMnemonic"
    | "invalidSeed"
    | "invalidAddress"
    | "invalidKeyPath"
    | "invalidBytes"
    | "keyMismatch"
    | "transfer";
  reason?: string;
  /** For `invalidBytes`: contract, proof, predicate, flavor or blinding. */
  what?: string;
  /** For `keyMismatch`: the input's position. */
  input?: number;
}
