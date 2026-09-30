import type { FlameBlock } from '../api/types';

export type Position = { column: number; lane: number };

export function layoutFlame(
  blocks: FlameBlock[],
  previous: Map<string, Position> = new Map(),
): Map<string, Position> {
  const positions = new Map(previous);
  const sorted = [...blocks].sort((a, b) => {
    const height = BigInt(a.tip.height) - BigInt(b.tip.height);
    return height < 0n ? -1 : height > 0n ? 1 : a.tip.hash.localeCompare(b.tip.hash);
  });
  let lastLane = Math.max(-1, ...Array.from(positions.values(), (position) => position.lane));
  for (const block of sorted) {
    if (positions.has(block.tip.hash)) continue;
    const parent = block.parent_hash ? positions.get(block.parent_hash) : undefined;
    const column = parent ? parent.column + 1 : 0;
    const laneTaken =
      parent &&
      Array.from(positions.values()).some(
        (position) => position.column === column && position.lane === parent.lane,
      );
    const lane = parent && !laneTaken ? parent.lane : ++lastLane;
    positions.set(block.tip.hash, { column, lane });
  }
  return positions;
}
