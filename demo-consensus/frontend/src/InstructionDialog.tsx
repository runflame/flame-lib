import { useEffect, useRef } from 'react';

export function InstructionDialog({ close }: { close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  return (
    <dialog
      ref={dialog}
      className="command-dialog instruction-dialog"
      aria-labelledby="instruction-title"
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
    >
      <div className="dialog-header">
        <span className="eyebrow">CONSENSUS LAB / INSTRUCTION</span>
        <button type="button" className="icon-button" aria-label="Close" onClick={close}>
          ×
        </button>
      </div>
      <h2 id="instruction-title">How this demo works</h2>
      <div className="instruction-content">
        <section>
          <h3>Two chains</h3>
          <p>
            Bitcoin confirms acquisitions and votes. Flame contains core blocks and competing
            branches. Select a block or transaction to see its details and status in the inspector.
          </p>
        </section>
        <section>
          <h3>Ready to start</h3>
          <p>
            Alice already has a 20,000-sat acquisition and an accepted vote for genesis core block
            #1 with Weight 7. Reset restores this starting point and discards the current session.
          </p>
        </section>
        <section className="instruction-steps">
          <h3>Try the next block</h3>
          <ol>
            <li>
              Click <strong>+ Flame block</strong>: use the current canonical tip as the parent and
              set Target BTC to the current Bitcoin height + 1.
            </li>
            <li>
              Select the new block and click <strong>Vote</strong> in Alice’s row.
            </li>
            <li>
              Click <strong>+1 Bitcoin block</strong>: the vote leaves the mempool, is processed,
              and adds weight to the block.
            </li>
          </ol>
        </section>
        <section>
          <h3>Minters and acquisitions</h3>
          <p>
            Create a minter and confirm its funding with a Bitcoin block. An acquisition sets the
            amount of sats used for voting; it also needs confirmation. Once confirmed at height H,
            it is active from H + 1 through H + 100, inclusive.
          </p>
          <p>
            What matters is whether the acquisition is active at{' '}
            <strong>the core block’s Target BTC</strong>. If no acquisition is active at that
            height, the vote is rejected, even if an acquisition is active now.
          </p>
        </section>
        <section>
          <h3>Votes, weight, and branch selection</h3>
          <p>
            A vote confirmed before Target BTC is rejected. A minter’s contribution is the sum of
            sats / duration for each active acquisition, rounded down individually. Each BTC block
            of delay halves that contribution, rounded down; a delay of more than 10 blocks makes it
            zero.
          </p>
          <p>
            Block weight is log₂ of the total contribution of accepted votes, rounded down; a zero
            total means Weight = 0. For example, 20,000 sats over 100 blocks gives Weight 7 for an
            on-time vote and 6 for a vote one block late. Chain weight is the sum of its block
            weights. Consensus selects the branch with the highest total weight as canonical. Weight
            on a block card is the chain weight up to and including that block.
          </p>
        </section>
        <section>
          <h3>Forks and double signing</h3>
          <p>
            To create a fork, create two blocks with the same parent and vote for them with
            different minters. One minter voting for different blocks at the same core height is a
            double sign: both votes lose their contribution, weights are recalculated, and further
            votes from that minter are rejected.
          </p>
        </section>
      </div>
      <div className="dialog-actions">
        <button type="button" className="button primary" onClick={close}>
          Got it
        </button>
      </div>
    </dialog>
  );
}
