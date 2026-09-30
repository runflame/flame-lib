import { parse, stringify } from 'lossless-json';
import type { Command, Snapshot } from './types';

const smallIntegers = new Set([
  'id',
  'minter_id',
  'core_height',
  'output_index',
  'duration_blocks',
  'target_btc_height',
  'block_weight',
  'acquisition_maturity',
  'default_acquisition_duration',
  'min_acquisition_duration',
  'max_vote_delay',
]);

export function decodeJson(text: string): unknown {
  return parse(
    text,
    (key, value) => (smallIntegers.has(key) && typeof value === 'string' ? Number(value) : value),
    { parseNumber: (value) => value },
  );
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly code: string,
    readonly status: number,
  ) {
    super(message);
  }
}

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`/api/${path}`, options);
  } catch (error) {
    if (error instanceof Error && error.name === 'AbortError') throw error;
    throw new ApiError(
      'Cannot connect to the backend. Make sure it is running on port 3001.',
      'network',
      0,
    );
  }
  const text = await response.text();
  let body: unknown;
  try {
    body = decodeJson(text);
  } catch {
    throw new ApiError(
      'The backend returned an invalid response. Check that it is available.',
      'invalid_response',
      response.status,
    );
  }
  if (!response.ok) {
    const error = (body as { error?: { message?: string; code?: string } })?.error;
    throw new ApiError(
      error?.message ?? `HTTP ${response.status}`,
      error?.code ?? 'http_error',
      response.status,
    );
  }
  return body as T;
}

export const getSnapshot = (signal?: AbortSignal) => request<Snapshot>('state', { signal });

const paths = {
  bitcoin: 'bitcoin/blocks',
  flame: 'flame/blocks',
  minter: 'minters',
  acquisition: 'acquisitions',
  vote: 'votes',
  reset: 'reset',
};

export function sendCommand(command: Command): Promise<unknown> {
  return request(paths[command.kind], {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: stringify(command.payload),
  });
}
