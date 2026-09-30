import { afterEach, describe, expect, it, vi } from 'vitest';
import { decodeJson, sendCommand } from './client';

afterEach(() => vi.unstubAllGlobals());

describe('API precision and mutations', () => {
  it('preserves u64 and u128 while keeping bounded identifiers numeric', () => {
    expect(
      decodeJson(
        '{"chain_weight":340282366920938463463374607431768211455,"amount_sats":18446744073709551615,"tip":{"height":9007199254740993},"minter_id":7,"core":{"height":1,"target_btc_height":105}}',
      ),
    ).toEqual({
      chain_weight: '340282366920938463463374607431768211455',
      amount_sats: '18446744073709551615',
      tip: { height: '9007199254740993' },
      minter_id: 7,
      core: { height: '1', target_btc_height: 105 },
    });
  });

  it('sends exact acquisition amounts as JSON numbers', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('{"txid":"abc"}', { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    await sendCommand({
      kind: 'acquisition',
      payload: { minter_id: 0, amount_sats: 18446744073709551615n },
    });
    expect(fetch.mock.calls[0][1].body).toBe('{"minter_id":0,"amount_sats":18446744073709551615}');
  });

  it('does not retry a command that might already have executed', async () => {
    const fetch = vi.fn().mockResolvedValue(
      new Response('{"error":{"code":"timeout","message":"consensus timed out"}}', {
        status: 504,
      }),
    );
    vi.stubGlobal('fetch', fetch);
    await expect(sendCommand({ kind: 'bitcoin', payload: { count: 1 } })).rejects.toThrow(
      'consensus timed out',
    );
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
