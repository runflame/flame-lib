import { describe, expect, it } from 'vitest';
import type { FlameBlock } from '../api/types';
import { layoutFlame } from './layout';

const block = (hash: string, parent: string | null, height: string): FlameBlock => ({
  tip: { hash, height },
  parent_hash: parent,
  core: null,
  is_canonical: false,
  parent_weight: '0',
  effective_power: '0',
  block_weight: 0,
  chain_weight: '0',
});

describe('Flame graph', () => {
  it('keeps existing nodes in place when a competing branch appears or weights change', () => {
    const root = block('root', null, '1');
    const first = block('z', 'root', '2');
    const child = block('child', 'z', '3');
    const initial = layoutFlame([root, first, child]);
    const fork = block('a', 'root', '2');
    const updated = layoutFlame(
      [root, { ...first, effective_power: '800', is_canonical: true }, child, fork],
      initial,
    );
    expect(updated.get('z')).toEqual(initial.get('z'));
    expect(updated.get('child')).toEqual(initial.get('child'));
    expect(updated.get('a')?.lane).not.toBe(updated.get('z')?.lane);
    expect(updated.get('a')?.column).toBe(updated.get('z')?.column);
  });

  it('places descendants from shuffled input without overlapping siblings', () => {
    const positions = layoutFlame([
      block('d', 'b', '3'),
      block('c', 'a', '2'),
      block('b', 'a', '2'),
      block('a', null, '1'),
    ]);
    expect(positions.get('d')?.column).toBe(2);
    expect(new Set(Array.from(positions.values(), (p) => `${p.column}:${p.lane}`)).size).toBe(4);
  });
});
