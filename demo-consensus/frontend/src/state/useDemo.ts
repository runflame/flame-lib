import { useEffect, useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { getSnapshot, sendCommand } from '../api/client';
import type { Command, Selection, Snapshot } from '../api/types';
import { selectionExists, sessionIdentity, shortHash } from './model';

export type Activity = { id: number; time: string; message: string; error?: boolean };
export const commandLabels: Record<Command['kind'], string> = {
  bitcoin: 'Creating BTC blocks',
  flame: 'Creating a Flame block',
  minter: 'Creating a minter',
  acquisition: 'Sending acquisition',
  vote: 'Sending vote',
  reset: 'Resetting session',
};
const queryKey = ['demo-state'];

export function useDemo() {
  const client = useQueryClient();
  const [selection, setSelection] = useState<Selection | null>(null);
  const [pending, setPending] = useState<Command['kind'] | null>(null);
  const [commandError, setCommandError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [activity, setActivity] = useState<Activity[]>([]);
  const busy = useRef(false);
  const sequence = useRef(0);
  const previous = useRef<Snapshot | undefined>(undefined);
  const query = useQuery({
    queryKey,
    queryFn: ({ signal }) => getSnapshot(signal),
    refetchInterval: pending ? false : 500,
    enabled: !pending,
    retry: false,
    refetchOnWindowFocus: !pending,
    structuralSharing: true,
  });

  function log(message: string, error = false) {
    const entry = {
      id: sequence.current++,
      time: new Date().toLocaleTimeString('en-US'),
      message,
      error,
    };
    setActivity((items) => [entry, ...items].slice(0, 100));
  }

  useEffect(() => {
    const data = query.data;
    if (!data) return;
    const old = previous.current;
    if (old && sessionIdentity(old) !== sessionIdentity(data)) {
      setSelection(null);
      setActivity([]);
      log('New session');
    } else if (
      old &&
      old.flame.canonical_tip?.hash !== data.flame.canonical_tip?.hash &&
      data.flame.canonical_tip
    ) {
      log(`Canonical tip → ${shortHash(data.flame.canonical_tip.hash)}`);
    }
    setSelection((current) => (current && selectionExists(current, data) ? current : null));
    previous.current = data;
  }, [query.data]);

  async function execute(command: Command): Promise<boolean> {
    if (busy.current) return false;
    busy.current = true;
    setPending(command.kind);
    setCommandError(null);
    setNotice(null);
    await client.cancelQueries({ queryKey });
    let succeeded = false;
    try {
      const result = await sendCommand(command);
      if (command.kind === 'reset') {
        setSelection(null);
        setActivity([]);
        previous.current = result as Snapshot;
        client.setQueryData(queryKey, result);
      }
      const txid = (result as { txid?: string }).txid;
      const message = txid
        ? `Transaction ${shortHash(txid)} sent. Confirm it with a BTC block.`
        : command.kind === 'minter'
          ? 'Minter created. Confirm its funding with a BTC block.'
          : `${commandLabels[command.kind]} — done`;
      setNotice(message);
      log(message);
      succeeded = true;
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Command failed';
      setCommandError(
        `${message} The state has been requested again: the command may have partially completed. Check the result before trying again.`,
      );
      log(message, true);
    } finally {
      await client
        .fetchQuery({
          queryKey,
          queryFn: ({ signal }) => getSnapshot(signal),
          retry: false,
          staleTime: 0,
        })
        .catch(() => undefined);
      setPending(null);
      busy.current = false;
    }
    return succeeded;
  }

  return { ...query, selection, setSelection, pending, commandError, notice, activity, execute };
}
