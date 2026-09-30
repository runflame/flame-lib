import { useEffect, useRef, useState } from 'react';
import type { Command, Selection, Snapshot } from '../api/types';
import { shortHash } from '../state/model';

type Props = {
  kind: Command['kind'];
  data: Snapshot;
  selection: Selection | null;
  initialMinterId?: number;
  pending: boolean;
  commandError: string | null;
  execute: (command: Command) => Promise<boolean>;
  close: () => void;
};
const titles = {
  bitcoin: 'Create Bitcoin blocks',
  flame: 'Create a Flame core block',
  minter: 'Create a minter',
  acquisition: 'Send acquisition',
  vote: 'Send vote',
  reset: 'Start a new session',
};

export function CommandDialog({
  kind,
  data,
  selection,
  initialMinterId,
  pending,
  commandError,
  execute,
  close,
}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const selectedBlock =
    selection?.kind === 'flame'
      ? data.flame.blocks.find((block) => block.tip.hash === selection.id)
      : undefined;
  const [parent, setParent] = useState(
    selectedBlock?.tip.hash ??
      data.flame.canonical_tip?.hash ??
      data.flame.blocks[0]?.tip.hash ??
      '',
  );
  const [blockHash, setBlockHash] = useState(
    selectedBlock?.core
      ? selectedBlock.tip.hash
      : (data.flame.blocks.find((block) => block.core)?.tip.hash ?? ''),
  );
  const [minterId, setMinterId] = useState(
    initialMinterId ?? (selection?.kind === 'minter' ? selection.id : (data.minters[0]?.id ?? 0)),
  );
  const [name, setName] = useState('');
  const [amount, setAmount] = useState('20000');
  const [count, setCount] = useState('1');
  const [target, setTarget] = useState(String(BigInt(data.bitcoin.tip.height) + 1n));
  const [error, setError] = useState('');
  const [failedRequest, setFailedRequest] = useState(false);
  const coreBlocks = data.flame.blocks.filter((block) => block.core);
  const votedBlock = coreBlocks.find((block) => block.tip.hash === blockHash);
  const doubleVote =
    votedBlock &&
    data.votes.some(
      (vote) =>
        vote.minter_id === minterId &&
        String(vote.core_height) === votedBlock.core?.height &&
        vote.block_hash !== blockHash,
    );

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    setError('');
    setFailedRequest(false);
    let command: Command;
    switch (kind) {
      case 'bitcoin': {
        const value = Number(count);
        if (!Number.isInteger(value) || value < 1 || value > 100)
          return setError('Block count must be a whole number from 1 to 100.');
        command = { kind, payload: { count: value } };
        break;
      }
      case 'flame': {
        const value = Number(target);
        if (!parent || !/^\d+$/.test(target) || !Number.isInteger(value) || value > 4294967295)
          return setError('Select a parent and a BTC height from 0 to 4,294,967,295.');
        command = { kind, payload: { parent_hash: parent, target_btc_height: value } };
        break;
      }
      case 'minter': {
        const value = name.trim();
        if (!value || Array.from(value).length > 80)
          return setError('Name must contain 1 to 80 characters.');
        command = { kind, payload: { name: value } };
        break;
      }
      case 'acquisition': {
        if (!/^\d+$/.test(amount) || BigInt(amount) <= 0n || BigInt(amount) > 18446744073709551615n)
          return setError('Enter a whole number of sats from 1 to 18,446,744,073,709,551,615.');
        command = { kind, payload: { minter_id: minterId, amount_sats: BigInt(amount) } };
        break;
      }
      case 'vote': {
        if (!votedBlock?.core) return setError('Create a Flame core block first.');
        command = {
          kind,
          payload: {
            minter_id: minterId,
            core_height: Number(votedBlock.core.height),
            block_hash: blockHash,
          },
        };
        break;
      }
      case 'reset':
        command = { kind, payload: {} };
        break;
    }
    if (await execute(command)) close();
    else {
      setFailedRequest(true);
      setError(
        'The command failed. See the main screen for details and check the state before trying again.',
      );
    }
  }

  return (
    <dialog
      ref={dialog}
      className="command-dialog"
      onCancel={(event) => {
        event.preventDefault();
        if (!pending) close();
      }}
    >
      <form onSubmit={submit}>
        <div className="dialog-header">
          <span className="eyebrow">CONSENSUS LAB / COMMAND</span>
          <button
            type="button"
            className="icon-button"
            aria-label="Close"
            disabled={pending}
            onClick={close}
          >
            ×
          </button>
        </div>
        <h2>{titles[kind]}</h2>
        <fieldset disabled={pending}>
          {kind === 'bitcoin' && (
            <>
              <label>
                Block count
                <input
                  autoFocus
                  type="number"
                  min="1"
                  max="100"
                  step="1"
                  required
                  value={count}
                  onChange={(event) => setCount(event.target.value)}
                />
              </label>
              <p className="help">
                Mempool transactions will be confirmed. The command completes once consensus has
                processed the blocks.
              </p>
            </>
          )}
          {kind === 'minter' && (
            <>
              <label>
                Minter name
                <input
                  autoFocus
                  required
                  value={name}
                  placeholder="For example, Bob"
                  onChange={(event) => setName(event.target.value)}
                />
              </label>
              <p className="help">
                The minter receives 50,000 sats to fund vote transactions. After creation, confirm
                the funding with a BTC block.
              </p>
            </>
          )}
          {kind === 'flame' && (
            <>
              <label>
                Parent Flame block
                <select
                  autoFocus
                  value={parent}
                  onChange={(event) => setParent(event.target.value)}
                >
                  {data.flame.blocks.map((block) => (
                    <option key={block.tip.hash} value={block.tip.hash}>
                      {block.core ? `Core #${block.core.height}` : 'Genesis'} ·{' '}
                      {shortHash(block.tip.hash)}
                      {block.is_canonical ? ' · canonical' : ''}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                Target BTC height
                <input
                  required
                  type="number"
                  min="0"
                  max="4294967295"
                  step="1"
                  value={target}
                  onChange={(event) => setTarget(event.target.value)}
                />
              </label>
              <p className="help">
                Use the same parent to create a competing branch. Current BTC tip: #
                {data.bitcoin.tip.height}.
              </p>
            </>
          )}
          {(kind === 'acquisition' || kind === 'vote') && (
            <label>
              Minter
              <select
                autoFocus
                value={minterId}
                onChange={(event) => setMinterId(Number(event.target.value))}
              >
                {data.minters.map((minter) => (
                  <option key={minter.id} value={minter.id}>
                    {minter.name}
                    {minter.is_double_signed ? ' · double sign' : ''}
                  </option>
                ))}
              </select>
            </label>
          )}
          {kind === 'acquisition' && (
            <>
              <label>
                Amount, sats
                <input
                  required
                  inputMode="numeric"
                  pattern="[0-9]+"
                  value={amount}
                  onChange={(event) => setAmount(event.target.value)}
                />
              </label>
              <p className="help">
                Duration: {data.consensus.parameters.default_acquisition_duration} BTC blocks.
                Maturity: {data.consensus.parameters.acquisition_maturity} BTC blocks. Paid by the
                regtest wallet.
              </p>
            </>
          )}
          {kind === 'vote' && (
            <>
              <label>
                Core block
                <select
                  value={blockHash}
                  onChange={(event) => setBlockHash(event.target.value)}
                  required
                >
                  <option value="" disabled>
                    Select a block
                  </option>
                  {coreBlocks.map((block) => (
                    <option key={block.tip.hash} value={block.tip.hash}>
                      Core #{block.core!.height} · {shortHash(block.tip.hash)}
                    </option>
                  ))}
                </select>
              </label>
              <p className="help">
                {votedBlock
                  ? `Core height: ${votedBlock.core!.height}. Target BTC: ${votedBlock.core!.target_btc_height}.`
                  : 'Create a core block first.'}{' '}
                After sending, create a BTC block to confirm the vote.
              </p>
              {doubleVote && (
                <div className="inline-warning">
                  This minter has already voted for another block at this core height. Sending this
                  vote demonstrates a double sign.
                </div>
              )}
            </>
          )}
          {kind === 'reset' && (
            <p className="help">
              The current chains, minters, and transactions will be replaced with a new session.
            </p>
          )}
        </fieldset>
        {error && (
          <div role="alert" className="inline-warning">
            {failedRequest && commandError ? commandError : error}
          </div>
        )}
        <div className="dialog-actions">
          <button type="button" className="button" disabled={pending} onClick={close}>
            Cancel
          </button>
          <button
            className={`button ${kind === 'reset' ? 'danger' : 'primary'}`}
            disabled={pending || (kind === 'vote' && !votedBlock)}
          >
            {pending ? 'Running…' : kind === 'reset' ? 'Reset session' : 'Run'}
          </button>
        </div>
      </form>
    </dialog>
  );
}
