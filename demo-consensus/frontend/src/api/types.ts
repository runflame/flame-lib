export type Decimal = string;
export type Tip = { hash: string; height: Decimal };
export type BitcoinBlock = { tip: Tip; parent_hash: string | null; transactions: string[] };
export type FlameBlock = {
  tip: Tip;
  parent_hash: string | null;
  core: { height: Decimal; target_btc_height: number } | null;
  is_canonical: boolean;
  parent_weight: Decimal;
  effective_power: Decimal;
  block_weight: number;
  chain_weight: Decimal;
};
export type Minter = {
  id: number;
  name: string;
  p2wsh_address: string;
  automatic_voting: false;
  is_double_signed: boolean;
};
export type TransactionStatus = { status: 'mempool' } | { status: 'confirmed'; block: Tip };
export type AcquisitionProcessing =
  | { status: 'unprocessed' | 'rejected' }
  | {
      status: 'accepted';
      activates_at_btc_height: Decimal;
      expires_at_btc_height_exclusive: Decimal;
    };
export type VoteProcessing =
  | { status: 'unprocessed' | 'pending_block' | 'invalidated_by_double_sign' | 'rejected' }
  | { status: 'accepted'; effective_minting_power: Decimal };
export type Acquisition = {
  txid: string;
  output_index: number;
  minter_id: number;
  amount_sats: Decimal;
  duration_blocks: number;
  transaction_status: TransactionStatus;
  processing_status: AcquisitionProcessing;
};
export type Vote = {
  txid: string;
  output_index: number;
  minter_id: number;
  core_height: number;
  block_hash: string;
  transaction_status: TransactionStatus;
  processing_status: VoteProcessing;
};
export type Snapshot = {
  bitcoin: { start_height: Decimal; tip: Tip; blocks: BitcoinBlock[]; mempool: string[] };
  flame: { canonical_tip: Tip | null; blocks: FlameBlock[] };
  minters: Minter[];
  acquisitions: Acquisition[];
  votes: Vote[];
  consensus: {
    btc_cursor: Tip;
    heaviest_tip: Tip | null;
    parameters: {
      acquisition_maturity: number;
      default_acquisition_duration: number;
      min_acquisition_duration: number;
      max_vote_delay: number;
    };
    double_signs: { minter_id: number; core_height: number; votes: Vote[] }[];
  };
};
export type Commands = {
  bitcoin: { count: number };
  flame: { parent_hash: string; target_btc_height: number };
  minter: { name: string };
  acquisition: { minter_id: number; amount_sats: bigint };
  vote: { minter_id: number; core_height: number; block_hash: string };
  reset: Record<string, never>;
};
export type Command = { [K in keyof Commands]: { kind: K; payload: Commands[K] } }[keyof Commands];
export type Selection =
  { kind: 'bitcoin' | 'flame' | 'transaction'; id: string } | { kind: 'minter'; id: number };
