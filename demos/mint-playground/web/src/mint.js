export const BUDGET_MIN = 100000;
export const BUDGET_MAX = 100000000;
export const BUDGET_STEP = 100000;
export const FEE = 1000;
export const SPARKS = 100000000;
export const REWARD = 50 * SPARKS;
export const UNIT_KEY = 'flame-mint-playground-unit-v1';
export const TIME_KEY = 'flame-mint-playground-time-v1';
export const ZOOM_KEY = 'flame-mint-playground-zoom-v2';
export const ZOOM_MIN = 10;
export const ZOOM_DEFAULT = 500;
export const BLOCKS_PER_DAY = 144;
export const DURATION_MIN = 144;
export const DURATION_MAX = 4464;
export const DELAY_MAX = 4464;
export const SATS_PER_BTC = 100000000;

// Typed-value fields next to the sliders. A typed number is parsed leniently
// (currency signs, unit words and thousands separators are ignored), converted
// to whole sats or blocks independently of the slider's range. The slider
// only displays the nearest available step. `null` means nothing usable was typed.
export function parseInputNumber(text) {
  const cleaned = String(text ?? '').replace(/[^0-9.]/g, '');
  if (!cleaned || cleaned === '.') return null;
  const value = Number(cleaned);
  return Number.isFinite(value) ? value : null;
}

export function snapToRange(value, min, max, step = 1) {
  if (value === null || !Number.isFinite(value)) return null;
  const snapped = Math.round(value / step) * step;
  return Math.min(max, Math.max(min, snapped));
}

export function inputToSats(value, unit = 'sat', btcUsd = null) {
  if (value === null) return null;
  if (unit === 'sat') return value;
  if (unit === 'BTC') return value * SATS_PER_BTC;
  if (!(Number.isFinite(btcUsd) && btcUsd > 0)) return null;
  return (value / btcUsd) * SATS_PER_BTC;
}

export function inputToBlocks(value, mode = 'blocks') {
  if (value === null) return null;
  return mode === 'days' ? value * BLOCKS_PER_DAY : value;
}

export function blockDate(block) {
  if (block < 0) return '—';
  const date = new Date(
    Date.UTC(2027, 0, 11) + Math.floor((block - 1) / BLOCKS_PER_DAY) * 86400000,
  );
  if (Number.isNaN(date.getTime())) return `Block ${new Intl.NumberFormat('en').format(block)}`;
  return new Intl.DateTimeFormat('en', { month: 'short', day: 'numeric', timeZone: 'UTC' }).format(
    date,
  );
}

export function formatDurationValue(blocks, mode = 'blocks') {
  const value = mode === 'days' ? blocks / BLOCKS_PER_DAY : blocks;
  return new Intl.NumberFormat('en', { maximumFractionDigits: 3 }).format(value);
}

export function durationUnit(blocks, mode = 'blocks') {
  const value = mode === 'days' ? blocks / BLOCKS_PER_DAY : blocks;
  return `${mode === 'days' ? 'day' : 'block'}${value === 1 ? '' : 's'}`;
}

export function formatDuration(blocks, mode = 'blocks') {
  return `${formatDurationValue(blocks, mode)} ${durationUnit(blocks, mode)}`;
}

export function formatMoney(sats, unit = 'sat', btcUsd = null, isRate = false, digits = null) {
  if (unit === 'USD' && !(Number.isFinite(btcUsd) && btcUsd > 0)) return '—';
  const value =
    unit === 'BTC' ? sats / SATS_PER_BTC : unit === 'USD' ? sats * (btcUsd / SATS_PER_BTC) : sats;
  const minimumFractionDigits = digits ?? (unit === 'BTC' ? 8 : unit === 'USD' ? 2 : 0);
  return new Intl.NumberFormat('en', {
    minimumFractionDigits,
    maximumFractionDigits: unit === 'USD' && isRate ? 6 : minimumFractionDigits,
    roundingMode: 'trunc',
  }).format(value);
}

export function parseBtcUsdQuote(quote) {
  const data = quote?.data;
  if (
    data?.base !== 'BTC' ||
    data?.currency !== 'USD' ||
    typeof data.amount !== 'string' ||
    !data.amount.trim()
  )
    throw new Error('Invalid BTC/USD quote');
  const price = Number(data.amount);
  if (!Number.isFinite(price) || price <= 0) throw new Error('Invalid BTC/USD quote');
  return price;
}

// Give the earliest blocks the remainder, matching the integer database schedule.
export function contributionFee(contribution) {
  return contribution.mode === 'delegating' ? 0 : FEE;
}

export function burnAt(contribution, block) {
  const offset = block - contribution.start_block;
  if (offset < 0 || offset >= contribution.duration) return 0;
  const burn = contribution.budget_sats - contribution.duration * contributionFee(contribution);
  return Math.floor(burn / contribution.duration) + (offset < burn % contribution.duration ? 1 : 0);
}

export function feeAt(contribution, block) {
  return block >= contribution.start_block &&
    block < contribution.start_block + contribution.duration
    ? contributionFee(contribution)
    : 0;
}

export function serviceIsActive(contributions, userId, block) {
  return contributions.some(
    (contribution) =>
      contribution.mode === 'service' &&
      contribution.user_id === userId &&
      block >= contribution.start_block &&
      block < contribution.start_block + contribution.duration,
  );
}

export function mintingBurnAt(contribution, block, contributions) {
  if (
    contribution.mode === 'delegating' &&
    !serviceIsActive(contributions, contribution.service_user_id, block)
  )
    return 0;
  return burnAt(contribution, block);
}

export function chartBlocks(contributions, firstBlock, lastBlock, heightNow, pending = null) {
  const count = lastBlock - firstBlock + 1;
  if (count <= 2001) return Array.from({ length: count }, (_, index) => firstBlock + index);
  // Rates only change at schedule boundaries; keep both sides so even one-block spikes survive zooming out.
  const blocks = new Set([firstBlock, lastBlock]);
  const add = (block) => {
    if (block >= firstBlock && block <= lastBlock) blocks.add(block);
  };
  add(heightNow);
  for (const fraction of [0.25, 0.5, 0.75]) add(firstBlock + (lastBlock - firstBlock) * fraction);
  const schedule = (contribution) => {
    const burn = contribution.budget_sats - contribution.duration * contributionFee(contribution);
    const remainder = burn % contribution.duration;
    const boundaries = [contribution.start_block, contribution.start_block + contribution.duration];
    if (remainder > 0) boundaries.push(contribution.start_block + remainder);
    for (const block of boundaries) {
      add(block - 1);
      add(block);
    }
  };
  contributions.forEach(schedule);
  if (pending) schedule(pending);
  return [...blocks].sort((left, right) => left - right);
}

export function projectedReward(contributions, draft) {
  const start = draft.start_block;
  const end = start + draft.duration;
  const events = new Map();
  const eventAt = (block) => {
    block = Math.max(start, Math.min(end, block));
    const event = events.get(block) ?? { burns: new Map(), services: new Map(), own: 0n };
    events.set(block, event);
    return event;
  };
  const add = (block, change, contribution, own) => {
    const event = eventAt(block);
    const provider = contribution.mode === 'delegating' ? contribution.service_user_id : 0;
    event.burns.set(provider, (event.burns.get(provider) ?? 0n) + change);
    if (own) event.own += change;
  };
  const schedule = (contribution, own = false) => {
    const duration = BigInt(contribution.duration);
    const burn =
      BigInt(contribution.budget_sats) - duration * BigInt(contributionFee(contribution));
    const rate = burn / duration;
    const remainder = Number(burn % duration);
    add(contribution.start_block, rate + (remainder > 0 ? 1n : 0n), contribution, own);
    if (remainder > 0) add(contribution.start_block + remainder, -1n, contribution, own);
    add(contribution.start_block + contribution.duration, -rate, contribution, own);
    if (contribution.mode === 'service') {
      for (const [block, delta] of [
        [contribution.start_block, 1],
        [contribution.start_block + contribution.duration, -1],
      ]) {
        const event = eventAt(block);
        event.services.set(
          contribution.user_id,
          (event.services.get(contribution.user_id) ?? 0) + delta,
        );
      }
    }
  };
  schedule(draft, true);
  contributions.forEach((contribution) => schedule(contribution));

  // Burn rates only change at schedule boundaries; keep sums exact before computing shares.
  let reward = 0;
  let previous = start;
  let total = 0n;
  let own = 0n;
  const delegated = new Map();
  const active = new Map();
  for (const [block, event] of [...events].sort(([left], [right]) => left - right)) {
    const available = draft.mode !== 'delegating' || (active.get(draft.service_user_id) ?? 0) > 0;
    if (total > 0n && available)
      reward += ((REWARD * Number(own)) / Number(total)) * (block - previous);
    for (const [provider, delta] of event.services) {
      const oldCount = active.get(provider) ?? 0;
      const count = oldCount + delta;
      if (oldCount === 0 && count > 0) total += delegated.get(provider) ?? 0n;
      if (oldCount > 0 && count === 0) total -= delegated.get(provider) ?? 0n;
      active.set(provider, count);
    }
    for (const [provider, delta] of event.burns) {
      if (provider === 0) total += delta;
      else {
        delegated.set(provider, (delegated.get(provider) ?? 0n) + delta);
        if ((active.get(provider) ?? 0) > 0) total += delta;
      }
    }
    own += event.own;
    previous = block;
  }
  const selfDelegating =
    Number.isSafeInteger(draft.user_id) &&
    draft.user_id > 0 &&
    draft.user_id === draft.service_user_id;
  return draft.mode === 'delegating' && !selfDelegating
    ? (reward * (10000 - (draft.commission_bps ?? 0))) / 10000
    : reward;
}

// Originals plus RGB midpoints, ordered for maximum minimum adjacent OKLab distance, including wrap.
export const PALETTE = [
  '#ffd800',
  '#ff6922',
  '#ffc200',
  '#ff4242',
  '#ff9c00',
  '#ffe400',
  '#ff9921',
  '#fff000',
  '#ffaf00',
  '#ff5632',
  '#ffcd00',
  '#ff8311',
];
export const BLUE_PALETTE = [
  '#64b5ff',
  '#2354d8',
  '#409bff',
  '#173a9e',
  '#2478ed',
  '#83c7ff',
  '#3865ba',
  '#a3d9ff',
  '#3289f5',
  '#2b46ad',
  '#51a8ff',
  '#2d68db',
];

export function userColor(userId, mode = 'independent') {
  const palette = mode === 'delegating' ? BLUE_PALETTE : PALETTE;
  return palette[userId % palette.length];
}

export function contributionStatus(contribution, height) {
  if (height < contribution.start_block) return 'scheduled';
  return height < contribution.start_block + contribution.duration - 1 ? 'active' : 'finished';
}
