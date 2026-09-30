import type { ReactNode } from 'react';
import type { Snapshot, TransactionStatus } from './api/types';
import { minterColor, minterName, shortHash } from './state/model';

export function Badge({ children, tone = '' }: { children: ReactNode; tone?: string }) {
  return <span className={`badge ${tone}`}>{children}</span>;
}

export function Hash({ value, full = false }: { value: string; full?: boolean }) {
  return (
    <span className={full ? 'hash full-hash' : 'hash'} title={value}>
      {full ? value : shortHash(value)}
    </span>
  );
}

export function MinterLabel({
  data,
  id,
  onClick,
}: {
  data: Snapshot;
  id: number;
  onClick?: () => void;
}) {
  const content = (
    <>
      <span className="minter-dot" style={{ background: minterColor(id) }} />
      {minterName(data, id)}
    </>
  );
  return onClick ? (
    <button className="text-button minter-label" onClick={onClick}>
      {content}
    </button>
  ) : (
    <span className="minter-label">{content}</span>
  );
}

export function TxStatus({ status }: { status: TransactionStatus }) {
  return status.status === 'mempool' ? (
    <Badge tone="amber">In mempool</Badge>
  ) : (
    <Badge>BTC #{status.block.height}</Badge>
  );
}

export function ProcessingStatus({ status }: { status: string }) {
  const labels: Record<string, string> = {
    unprocessed: 'Unprocessed',
    accepted: 'Accepted',
    rejected: 'Rejected',
    pending_block: 'Waiting for core block',
    invalidated_by_double_sign: 'Invalidated: double sign',
  };
  return (
    <Badge
      tone={
        status === 'accepted'
          ? 'green'
          : status === 'rejected' || status === 'invalidated_by_double_sign'
            ? 'red'
            : ''
      }
    >
      {labels[status] ?? status}
    </Badge>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="empty">{children}</div>;
}

export function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="detail">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}
