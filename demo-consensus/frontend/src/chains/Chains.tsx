import { useEffect, useMemo, useRef } from 'react';
import type { Selection, Snapshot } from '../api/types';
import { Badge, Hash } from '../components';
import { integer, sessionIdentity } from '../state/model';
import { layoutFlame, type Position } from './layout';

type Props = {
  data: Snapshot;
  selection: Selection | null;
  select: (selection: Selection) => void;
  disabled: boolean;
  createBlock: () => void;
};

export function BitcoinChain({ data, selection, select, disabled, createBlock }: Props) {
  const viewport = useRef<HTMLDivElement>(null);
  const followingTip = useRef(true);
  const session = sessionIdentity(data);
  useEffect(() => {
    followingTip.current = true;
  }, [session]);
  useEffect(() => {
    if (followingTip.current && viewport.current)
      viewport.current.scrollLeft = viewport.current.scrollWidth;
  }, [data.bitcoin.tip.hash, session]);
  const selectedVotes =
    selection?.kind === 'flame'
      ? data.votes.filter((vote) => vote.block_hash === selection.id)
      : [];
  const relatedTransactions = new Set(selectedVotes.map((vote) => vote.txid));
  const blocks = data.bitcoin.blocks.filter(
    (block) => BigInt(block.tip.height) >= BigInt(data.bitcoin.start_height),
  );
  return (
    <section className="panel chain-panel">
      <div className="panel-heading">
        <div className="chain-title">
          <span className="chain-symbol bitcoin">₿</span>
          <div>
            <h2>Bitcoin</h2>
            <span className="subtle">Regtest · transaction confirmations</span>
          </div>
        </div>
        <div className="chain-controls">
          <button
            className="text-button subtle"
            onClick={() => {
              followingTip.current = true;
              if (viewport.current) viewport.current.scrollLeft = viewport.current.scrollWidth;
            }}
          >
            To tip →
          </button>
          <Badge tone="amber">Blocks: {blocks.length}</Badge>
          <button className="button bitcoin-button" disabled={disabled} onClick={createBlock}>
            +1 Bitcoin block
          </button>
        </div>
      </div>
      <div
        className="bitcoin-scroll"
        ref={viewport}
        onScroll={(event) => {
          const element = event.currentTarget;
          followingTip.current =
            element.scrollWidth - element.scrollLeft - element.clientWidth < 32;
        }}
      >
        <div className="bitcoin-track">
          {blocks.map((block) => {
            const acquisitions = data.acquisitions.filter((item) =>
              block.transactions.includes(item.txid),
            ).length;
            const votes = data.votes.filter((item) =>
              block.transactions.includes(item.txid),
            ).length;
            const related = block.transactions.some((txid) => relatedTransactions.has(txid));
            return (
              <button
                key={block.tip.hash}
                className={`block-card btc-card ${selection?.kind === 'bitcoin' && selection.id === block.tip.hash ? 'selected' : ''} ${related ? 'related' : ''}`}
                onClick={() => select({ kind: 'bitcoin', id: block.tip.hash })}
              >
                <span className="block-top">
                  <strong>#{block.tip.height}</strong>
                  {block.tip.hash === data.bitcoin.tip.hash && (
                    <span className="tip-label">TIP</span>
                  )}
                </span>
                <Hash value={block.tip.hash} />
                <span className="block-meta">
                  {acquisitions} acquisition · {votes} vote
                </span>
                <span className="block-footer">
                  {block.transactions.length} transactions <span>↗</span>
                </span>
              </button>
            );
          })}
        </div>
      </div>
      <div className="mempool">
        <span className="eyebrow">
          MEMPOOL <b>{data.bitcoin.mempool.length}</b>
        </span>
        <div className="mempool-items">
          {data.bitcoin.mempool.length ? (
            data.bitcoin.mempool.map((txid) => (
              <button
                key={txid}
                className={`tx-chip ${relatedTransactions.has(txid) ? 'related' : ''}`}
                onClick={() => select({ kind: 'transaction', id: txid })}
              >
                <span className="pulse-dot" />
                <Hash value={txid} />
              </button>
            ))
          ) : (
            <span className="subtle">Empty · send an acquisition or vote</span>
          )}
        </div>
      </div>
    </section>
  );
}

export function FlameChain({ data, selection, select, disabled, createBlock }: Props) {
  const cache = useRef<{ session: string; positions: Map<string, Position> }>({
    session: '',
    positions: new Map(),
  });
  const positions = useMemo(() => {
    const session = sessionIdentity(data);
    const previous =
      cache.current.session === session ? cache.current.positions : new Map<string, Position>();
    const result = layoutFlame(data.flame.blocks, previous);
    cache.current = { session, positions: result };
    return result;
  }, [data]);
  const width = (Math.max(0, ...Array.from(positions.values(), (p) => p.column)) + 1) * 220 + 32;
  const height = (Math.max(0, ...Array.from(positions.values(), (p) => p.lane)) + 1) * 162 + 32;
  return (
    <section className="panel chain-panel flame-panel">
      <div className="panel-heading">
        <div className="chain-title">
          <span className="chain-symbol flame">ϟ</span>
          <div>
            <h2>Flame</h2>
            <span className="subtle">Core blocks · branch selection by weight</span>
          </div>
        </div>
        <div className="chain-controls">
          <div className="legend">
            <span className="legend-line" />
            Canonical <span className="legend-line alternative" />
            Fork
          </div>
          <button className="button flame-button" disabled={disabled} onClick={createBlock}>
            + Flame block
          </button>
        </div>
      </div>
      <div className="flame-scroll">
        <div className="flame-canvas" style={{ width, height }}>
          <svg className="chain-edges" width={width} height={height} aria-hidden="true">
            {data.flame.blocks.map((block) => {
              const parent = block.parent_hash ? positions.get(block.parent_hash) : undefined;
              const child = positions.get(block.tip.hash);
              if (!parent || !child) return null;
              const x = parent.column * 220 + 208;
              const y = parent.lane * 162 + 84;
              const endX = child.column * 220 + 16;
              const endY = child.lane * 162 + 84;
              return (
                <path
                  key={block.tip.hash}
                  d={`M ${x} ${y} C ${x + 20} ${y}, ${endX - 20} ${endY}, ${endX} ${endY}`}
                  className={block.is_canonical ? 'canonical-edge' : 'fork-edge'}
                />
              );
            })}
          </svg>
          {data.flame.blocks.map((block) => {
            const position = positions.get(block.tip.hash)!;
            const heaviest = data.consensus.heaviest_tip?.hash === block.tip.hash;
            return (
              <button
                key={block.tip.hash}
                style={{ left: position.column * 220 + 16, top: position.lane * 162 + 16 }}
                className={`block-card flame-card ${block.is_canonical ? 'canonical' : ''} ${selection?.kind === 'flame' && selection.id === block.tip.hash ? 'selected' : ''}`}
                onClick={() => select({ kind: 'flame', id: block.tip.hash })}
              >
                <span className="block-top">
                  <strong>{block.core ? `Core #${block.core.height}` : 'Genesis'}</strong>
                  {heaviest ? (
                    <span className="tip-label">HEAVIEST</span>
                  ) : block.is_canonical ? (
                    <span className="tip-label">CANONICAL</span>
                  ) : (
                    <span className="subtle">FORK</span>
                  )}
                </span>
                <Hash value={block.tip.hash} />
                <span className="block-meta">
                  {block.core ? `Target BTC #${block.core.target_btc_height}` : 'Initial state'}
                </span>
                <span className="block-footer">
                  <span>
                    Weight <b>{integer(block.chain_weight)}</b>
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      </div>
      <div className="panel-footnote">
        Select a block to see its votes and related BTC confirmations.
      </div>
    </section>
  );
}
