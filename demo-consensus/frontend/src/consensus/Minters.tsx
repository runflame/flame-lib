import type { Acquisition, Command, Selection, Snapshot } from '../api/types';
import { Badge, MinterLabel, ProcessingStatus, TxStatus } from '../components';
import { acquisitionActivity, integer } from '../state/model';

type Props = {
  data: Snapshot;
  selection: Selection | null;
  select: (selection: Selection) => void;
  disabled: boolean;
  openCommand: (kind: Command['kind'], minterId?: number) => void;
};

function AcquisitionItem({
  acquisition,
  target,
  select,
}: {
  acquisition: Acquisition;
  target: string | number;
  select: Props['select'];
}) {
  const processing = acquisition.processing_status;
  const activity = acquisitionActivity(acquisition, target);
  return (
    <li>
      <button
        className="acquisition-item"
        onClick={() => select({ kind: 'transaction', id: acquisition.txid })}
        aria-label={`Acquisition ${integer(acquisition.amount_sats)} sats, ${acquisition.txid}`}
      >
        <span className="acquisition-summary">
          <strong>{integer(acquisition.amount_sats)} sats</strong>
          {processing.status === 'accepted' ? (
            <Badge tone={activity === 'Active' ? 'green' : 'amber'}>{activity}</Badge>
          ) : acquisition.transaction_status.status === 'mempool' ? (
            <TxStatus status={acquisition.transaction_status} />
          ) : (
            <ProcessingStatus status={processing.status} />
          )}
        </span>
        <span className="subtle">
          {processing.status === 'accepted'
            ? `BTC [${processing.activates_at_btc_height}, ${processing.expires_at_btc_height_exclusive})`
            : `${acquisition.duration_blocks} BTC blocks`}
        </span>
      </button>
    </li>
  );
}

export function Minters({ data, selection, select, disabled, openCommand }: Props) {
  const selectedBlock =
    selection?.kind === 'flame'
      ? data.flame.blocks.find((block) => block.tip.hash === selection.id)
      : undefined;
  const target = selectedBlock?.core?.target_btc_height ?? data.bitcoin.tip.height;

  return (
    <section className="panel minters-panel" aria-labelledby="minters-heading">
      <div className="panel-heading">
        <div>
          <h2 id="minters-heading">Minters</h2>
          <span className="subtle">
            Acquisition activity at {selectedBlock?.core ? 'target BTC' : 'BTC'} #{target}
          </span>
        </div>
        <button className="button" disabled={disabled} onClick={() => openCommand('minter')}>
          + Minter
        </button>
      </div>
      <ul className="minters-list">
        {data.minters.map((minter) => {
          const acquisitions = data.acquisitions.filter((item) => item.minter_id === minter.id);
          return (
            <li className="minter-row" key={minter.id} aria-label={`Minter ${minter.name}`}>
              <MinterLabel
                data={data}
                id={minter.id}
                onClick={() => select({ kind: 'minter', id: minter.id })}
              />
              {acquisitions.length ? (
                <ul className="minter-acquisitions">
                  {acquisitions.map((acquisition) => (
                    <AcquisitionItem
                      key={`${acquisition.txid}:${acquisition.output_index}`}
                      acquisition={acquisition}
                      target={target}
                      select={select}
                    />
                  ))}
                </ul>
              ) : (
                <span className="subtle">No acquisitions</span>
              )}
              <div className="minter-actions">
                <button
                  className="button"
                  disabled={disabled}
                  onClick={() => openCommand('acquisition', minter.id)}
                >
                  + Acquisition
                </button>
                <button
                  className="button"
                  disabled={disabled}
                  onClick={() => openCommand('vote', minter.id)}
                >
                  Vote
                </button>
              </div>
              <div className="minter-status" aria-label="Minter status">
                <Badge tone={minter.is_double_signed ? 'red' : 'green'}>
                  {minter.is_double_signed ? 'Double sign' : 'No double sign'}
                </Badge>
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
