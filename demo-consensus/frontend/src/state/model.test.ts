import { describe, expect, it } from 'vitest';
import type { Acquisition } from '../api/types';
import { acquisitionActivity } from './model';

describe('acquisition activity at the selected target height', () => {
  const acquisition: Acquisition = {
    txid: 'a',
    output_index: 0,
    minter_id: 0,
    amount_sats: '20000',
    duration_blocks: 100,
    transaction_status: { status: 'confirmed', block: { hash: 'b', height: '104' } },
    processing_status: {
      status: 'accepted',
      activates_at_btc_height: '105',
      expires_at_btc_height_exclusive: '205',
    },
  };
  it('uses inclusive maturity and exclusive expiry bounds', () => {
    expect(acquisitionActivity(acquisition, 104)).toBe('Maturing');
    expect(acquisitionActivity(acquisition, 105)).toBe('Active');
    expect(acquisitionActivity(acquisition, 204)).toBe('Active');
    expect(acquisitionActivity(acquisition, 205)).toBe('Expired');
  });
});
