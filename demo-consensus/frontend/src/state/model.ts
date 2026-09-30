import type { Acquisition, Selection, Snapshot } from '../api/types';

export const shortHash = (hash: string) => `${hash.slice(0, 7)}…${hash.slice(-4)}`;
export const integer = (value: string | number) => BigInt(value).toLocaleString('en-US');
export const minterColor = (id: number) =>
  ['#9fbcfa', '#c2a0ef', '#6bcebd', '#e8b36b', '#ee8eaa'][id % 5];
export const minterName = (data: Snapshot, id: number) =>
  data.minters.find((minter) => minter.id === id)?.name ?? `Minter ${id}`;

export function acquisitionActivity(acquisition: Acquisition, target: string | number): string {
  const status = acquisition.processing_status;
  if (status.status !== 'accepted') return 'Inactive';
  const height = BigInt(target);
  if (height < BigInt(status.activates_at_btc_height)) return 'Maturing';
  if (height >= BigInt(status.expires_at_btc_height_exclusive)) return 'Expired';
  return 'Active';
}

export function selectionExists(selection: Selection, data: Snapshot): boolean {
  switch (selection.kind) {
    case 'flame':
      return (
        data.flame.blocks.some((block) => block.tip.hash === selection.id) ||
        data.votes.some((vote) => vote.block_hash === selection.id)
      );
    case 'bitcoin':
      return data.bitcoin.blocks.some((block) => block.tip.hash === selection.id);
    case 'minter':
      return data.minters.some((minter) => minter.id === selection.id);
    case 'transaction':
      return (
        data.bitcoin.mempool.includes(selection.id) ||
        data.bitcoin.blocks.some((block) => block.transactions.includes(selection.id))
      );
  }
}

export function sessionIdentity(data: Snapshot): string {
  return `${data.minters[0]?.p2wsh_address ?? ''}:${data.bitcoin.blocks[0]?.tip.hash ?? ''}`;
}
