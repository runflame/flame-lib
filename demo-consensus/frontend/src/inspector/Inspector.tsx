import type { Command, Selection, Snapshot, Vote } from '../api/types';
import { Badge, Detail, Empty, Hash, MinterLabel, ProcessingStatus, TxStatus } from '../components';
import { integer, minterName } from '../state/model';

type Props = {
  data: Snapshot;
  selection: Selection | null;
  select: (selection: Selection) => void;
  openCommand: (kind: Command['kind']) => void;
  disabled: boolean;
};

function VoteDetails({
  data,
  votes,
  select,
}: {
  data: Snapshot;
  votes: Vote[];
  select: Props['select'];
}) {
  return (
    <div className="inspector-list">
      {votes.map((vote) => (
        <div className="inspector-item" key={`${vote.txid}:${vote.output_index}:${vote.minter_id}`}>
          <div className="item-heading">
            <MinterLabel
              data={data}
              id={vote.minter_id}
              onClick={() => select({ kind: 'minter', id: vote.minter_id })}
            />
          </div>
          <button
            className="text-button"
            onClick={() => select({ kind: 'transaction', id: vote.txid })}
          >
            <Hash value={vote.txid} />
          </button>
          <div className="badge-row">
            <TxStatus status={vote.transaction_status} />
            <ProcessingStatus status={vote.processing_status.status} />
          </div>
          {vote.transaction_status.status === 'confirmed' && (
            <button
              className="text-button muted"
              onClick={() =>
                vote.transaction_status.status === 'confirmed' &&
                select({ kind: 'bitcoin', id: vote.transaction_status.block.hash })
              }
            >
              Open BTC #{vote.transaction_status.block.height} ↗
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

export function Inspector({ data, selection, select, openCommand, disabled }: Props) {
  const block =
    selection?.kind === 'flame'
      ? data.flame.blocks.find((block) => block.tip.hash === selection.id)
      : undefined;
  const bitcoin =
    selection?.kind === 'bitcoin'
      ? data.bitcoin.blocks.find((block) => block.tip.hash === selection.id)
      : undefined;
  const minter =
    selection?.kind === 'minter'
      ? data.minters.find((minter) => minter.id === selection.id)
      : undefined;
  const txid = selection?.kind === 'transaction' ? selection.id : undefined;
  const txVotes = txid ? data.votes.filter((vote) => vote.txid === txid) : [];
  const acquisitions = txid
    ? data.acquisitions.filter((item) => item.txid === txid)
    : minter
      ? data.acquisitions.filter((item) => item.minter_id === minter.id)
      : [];
  const confirmedIn = txid
    ? data.bitcoin.blocks.find((item) => item.transactions.includes(txid))
    : undefined;
  const votes = block
    ? data.votes.filter((vote) => vote.block_hash === block.tip.hash)
    : minter
      ? data.votes.filter((vote) => vote.minter_id === minter.id)
      : txVotes;
  return (
    <aside className="panel inspector">
      <div className="panel-heading">
        <div>
          <span className="eyebrow">INSPECTOR</span>
          <h2>
            {block
              ? block.core
                ? `Core #${block.core.height}`
                : 'Genesis'
              : bitcoin
                ? `Bitcoin #${bitcoin.tip.height}`
                : minter
                  ? minter.name
                  : txid
                    ? 'Transaction'
                    : 'Consensus details'}
          </h2>
        </div>
        <span className="inspector-icon">⌖</span>
      </div>
      <div className="inspector-body">
        {!selection && (
          <div className="inspector-intro">
            <div className="inspect-orbit">⌖</div>
            <h3>Select a block or transaction</h3>
            <p>
              Select a block, minter, or transaction to see its connections, votes, and contribution
              to consensus.
            </p>
          </div>
        )}
        {selection?.kind === 'flame' && !block && (
          <Empty>This core block is not yet known. The vote may be waiting for it.</Empty>
        )}
        {block && (
          <>
            <Hash value={block.tip.hash} full />
            <div className="badge-row">
              {block.is_canonical && <Badge tone="green">Canonical</Badge>}
              {data.consensus.heaviest_tip?.hash === block.tip.hash && (
                <Badge tone="red">Heaviest</Badge>
              )}
            </div>
            <dl>
              <Detail label="Flame height">{block.tip.height}</Detail>
              <Detail label="Core height">{block.core?.height ?? '—'}</Detail>
              <Detail label="Target BTC">{block.core?.target_btc_height ?? '—'}</Detail>
              <Detail label="Parent weight">{integer(block.parent_weight)}</Detail>
              <Detail label="Block weight">{integer(block.block_weight)}</Detail>
              <Detail label="Chain weight">{integer(block.chain_weight)}</Detail>
              {block.parent_hash && (
                <Detail label="Parent">
                  <button
                    className="text-button"
                    onClick={() => select({ kind: 'flame', id: block.parent_hash! })}
                  >
                    <Hash value={block.parent_hash} />
                  </button>
                </Detail>
              )}
            </dl>
            <div className="inspector-actions">
              <button className="button" disabled={disabled} onClick={() => openCommand('flame')}>
                + Child block
              </button>
              {block.core && (
                <button
                  className="button primary"
                  disabled={disabled}
                  onClick={() => openCommand('vote')}
                >
                  Send vote
                </button>
              )}
            </div>
            <h3>
              Votes <span className="subtle">{votes.length}</span>
            </h3>
            {votes.length ? (
              <VoteDetails data={data} votes={votes} select={select} />
            ) : (
              <Empty>No votes for this block yet.</Empty>
            )}
          </>
        )}
        {bitcoin && (
          <>
            <Hash value={bitcoin.tip.hash} full />
            <dl>
              <Detail label="Height">{bitcoin.tip.height}</Detail>
              <Detail label="Transactions">{bitcoin.transactions.length}</Detail>
              {bitcoin.parent_hash && (
                <Detail label="Parent">
                  <Hash value={bitcoin.parent_hash} />
                </Detail>
              )}
            </dl>
            <h3>Transactions</h3>
            <div className="inspector-list">
              {bitcoin.transactions.map((tx) => (
                <button
                  className="transaction-row"
                  key={tx}
                  onClick={() => select({ kind: 'transaction', id: tx })}
                >
                  <Hash value={tx} />
                  <span className="subtle">
                    {data.votes.some((item) => item.txid === tx)
                      ? 'Vote'
                      : data.acquisitions.some((item) => item.txid === tx)
                        ? 'Acquisition'
                        : 'Bitcoin tx'}{' '}
                    ↗
                  </span>
                </button>
              ))}
            </div>
          </>
        )}
        {minter && (
          <>
            {minter.is_double_signed && (
              <div className="badge-row">
                <Badge tone="red">Double sign</Badge>
              </div>
            )}
            <dl>
              <Detail label="ID">{minter.id}</Detail>
            </dl>
            <span className="eyebrow">P2WSH ADDRESS</span>
            <div className="hash full-hash address">{minter.p2wsh_address}</div>
            <div className="inspector-actions">
              <button
                className="button"
                disabled={disabled}
                onClick={() => openCommand('acquisition')}
              >
                Acquisition
              </button>
              <button
                className="button primary"
                disabled={disabled}
                onClick={() => openCommand('vote')}
              >
                Vote
              </button>
            </div>
            <h3>Acquisitions</h3>
            {!acquisitions.length && <Empty>No acquisitions.</Empty>}
          </>
        )}
        {txid && (
          <>
            <Hash value={txid} full />
            <div className="badge-row">
              {confirmedIn ? (
                <button
                  className="text-button"
                  onClick={() => select({ kind: 'bitcoin', id: confirmedIn.tip.hash })}
                >
                  <Badge tone="green">BTC #{confirmedIn.tip.height} ↗</Badge>
                </button>
              ) : data.bitcoin.mempool.includes(txid) ? (
                <Badge tone="amber">In mempool</Badge>
              ) : (
                <Badge>Not in the current snapshot</Badge>
              )}
            </div>
            {!txVotes.length && !acquisitions.length && (
              <p className="help">
                Bitcoin transaction: funding, coinbase, or another operation without an acquisition
                or vote.
              </p>
            )}
          </>
        )}
        {acquisitions.map((acquisition) => (
          <div className="inspector-item" key={`${acquisition.txid}:${acquisition.output_index}`}>
            <div className="item-heading">
              <strong>{integer(acquisition.amount_sats)} sats</strong>
              <span>{acquisition.duration_blocks} blocks</span>
            </div>
            <MinterLabel
              data={data}
              id={acquisition.minter_id}
              onClick={() => select({ kind: 'minter', id: acquisition.minter_id })}
            />
            <button
              className="text-button"
              onClick={() => select({ kind: 'transaction', id: acquisition.txid })}
            >
              <Hash value={acquisition.txid} />:{acquisition.output_index}
            </button>
            <div className="badge-row">
              <TxStatus status={acquisition.transaction_status} />
              <ProcessingStatus status={acquisition.processing_status.status} />
            </div>
            {acquisition.processing_status.status === 'accepted' && (
              <p className="help">
                Active at target BTC [{acquisition.processing_status.activates_at_btc_height},{' '}
                {acquisition.processing_status.expires_at_btc_height_exclusive}).
              </p>
            )}
          </div>
        ))}
        {minter && (
          <>
            <h3>Votes</h3>
            <VoteDetails data={data} votes={votes} select={select} />
          </>
        )}
        {txid &&
          txVotes.map((vote) => (
            <div key={`${vote.output_index}:${vote.minter_id}`}>
              <h3>Vote · {minterName(data, vote.minter_id)}</h3>
              <button
                className="text-button"
                onClick={() => select({ kind: 'flame', id: vote.block_hash })}
              >
                Core #{vote.core_height} · <Hash value={vote.block_hash} /> ↗
              </button>
              <VoteDetails data={data} votes={[vote]} select={select} />
            </div>
          ))}
        <div className="protocol-parameters">
          <span className="eyebrow">PROTOCOL PARAMETERS</span>
          <dl>
            <Detail label="Acquisition maturity">
              {data.consensus.parameters.acquisition_maturity} BTC
            </Detail>
            <Detail label="Default duration">
              {data.consensus.parameters.default_acquisition_duration} BTC
            </Detail>
            <Detail label="Min duration">
              {data.consensus.parameters.min_acquisition_duration} BTC
            </Detail>
            <Detail label="Max vote delay">{data.consensus.parameters.max_vote_delay} BTC</Detail>
          </dl>
        </div>
      </div>
    </aside>
  );
}
