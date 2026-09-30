import { useState } from 'react';
import type { Command, Selection } from './api/types';
import { BitcoinChain, FlameChain } from './chains/Chains';
import { CommandDialog } from './commands/CommandDialog';
import { Badge } from './components';
import { Minters } from './consensus/Minters';
import { Inspector } from './inspector/Inspector';
import { InstructionDialog } from './InstructionDialog';
import { commandLabels, useDemo } from './state/useDemo';
import './styles.css';

export default function App() {
  const demo = useDemo();
  const [dialog, setDialog] = useState<{ kind: Command['kind']; minterId?: number } | null>(null);
  const [instructionOpen, setInstructionOpen] = useState(false);
  const data = demo.data;
  const disabled = !!demo.pending || !data || demo.isError;
  const synced = data?.bitcoin.tip.hash === data?.consensus.btc_cursor.hash;
  function openCommand(kind: Command['kind'], minterId?: number) {
    setDialog({ kind, minterId });
  }
  function select(selection: Selection) {
    demo.setSelection(selection);
  }

  return (
    <div className="app-shell">
      <header className="app-header">
        <div className="brand">
          <span className="brand-mark">ϟ</span>
          <strong>
            flame<span className="brand-divider">/</span>
            <span className="brand-product">consensus lab</span>
          </strong>
          <Badge>LOCAL REGTEST</Badge>
        </div>
        <div className="header-actions">
          <div className="connection">
            <span
              className={`connection-dot ${demo.isError ? 'offline' : demo.pending ? 'working' : ''}`}
            />
            {demo.isError
              ? 'Disconnected'
              : demo.pending
                ? 'Command in progress'
                : data
                  ? synced
                    ? 'Synced'
                    : 'Syncing'
                  : 'Connecting…'}
          </div>
          <button
            className="button"
            aria-haspopup="dialog"
            onClick={() => setInstructionOpen(true)}
          >
            Instruction
          </button>
          <button
            className="button reset-button"
            disabled={disabled}
            onClick={() => openCommand('reset')}
          >
            ↺ Reset
          </button>
        </div>
      </header>
      <main>
        <div className="feedback" aria-live="polite">
          {demo.pending && (
            <div className="notice pending">
              <span className="spinner" />
              {commandLabels[demo.pending]}…
            </div>
          )}
          {demo.commandError && (
            <div className="notice error" role="alert">
              {demo.commandError}
            </div>
          )}
          {demo.isError && (
            <div className="notice error" role="alert">
              {demo.error.message}
              {data &&
                ` Last snapshot: ${new Date(demo.dataUpdatedAt).toLocaleTimeString('en-US')}. Data may be out of date.`}
              <button
                className="text-button"
                disabled={!!demo.pending}
                onClick={() => void demo.refetch()}
              >
                Refresh ↻
              </button>
            </div>
          )}
          {demo.notice && !demo.pending && <div className="notice">{demo.notice}</div>}
        </div>
        {!data ? (
          <div className="panel startup">
            <div className="inspect-orbit">ϟ</div>
            <h2>{demo.isError ? 'Start the local backend' : 'Connecting to the chains'}</h2>
            <p>API expected at 127.0.0.1:3001</p>
            <code>cargo run -p demo-consensus-backend</code>
          </div>
        ) : (
          <>
            <div className="workspace">
              <div className="chains">
                <BitcoinChain
                  data={data}
                  selection={demo.selection}
                  select={select}
                  disabled={disabled}
                  createBlock={() => void demo.execute({ kind: 'bitcoin', payload: { count: 1 } })}
                />
                <FlameChain
                  data={data}
                  selection={demo.selection}
                  select={select}
                  disabled={disabled}
                  createBlock={() => openCommand('flame')}
                />
              </div>
              <Inspector
                data={data}
                selection={demo.selection}
                select={select}
                openCommand={openCommand}
                disabled={disabled}
              />
            </div>
            <Minters
              data={data}
              selection={demo.selection}
              select={select}
              disabled={disabled}
              openCommand={openCommand}
            />
            <section className="panel activity-panel">
              <div className="panel-heading">
                <div>
                  <h2>Activity log</h2>
                  <span className="subtle">
                    Commands and canonical tip changes since this page was opened
                  </span>
                </div>
                <Badge>{demo.activity.length}</Badge>
              </div>
              <div className="activity-list">
                {demo.activity.length ? (
                  demo.activity.map((entry) => (
                    <div
                      className={`activity-entry ${entry.error ? 'activity-error' : ''}`}
                      key={entry.id}
                    >
                      <time>{entry.time}</time>
                      <span>{entry.message}</span>
                    </div>
                  ))
                ) : (
                  <div className="empty">Start by creating a minter or sending an acquisition.</div>
                )}
              </div>
            </section>
          </>
        )}
        <footer>
          <span>FLAME / CONSENSUS LAB</span>
          <span>In-memory Flame · Bitcoin Core regtest · refreshes every 500 ms</span>
        </footer>
      </main>
      {instructionOpen && <InstructionDialog close={() => setInstructionOpen(false)} />}
      {dialog && data && (
        <CommandDialog
          key={`${dialog.kind}:${dialog.minterId ?? ''}`}
          kind={dialog.kind}
          initialMinterId={dialog.minterId}
          data={data}
          selection={demo.selection}
          pending={!!demo.pending}
          commandError={demo.commandError}
          execute={demo.execute}
          close={() => setDialog(null)}
        />
      )}
    </div>
  );
}
