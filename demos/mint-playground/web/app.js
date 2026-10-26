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
const SATS_PER_BTC = 100000000;

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
  return value / btcUsd * SATS_PER_BTC;
}

export function inputToBlocks(value, mode = 'blocks') {
  if (value === null) return null;
  return mode === 'days' ? value * BLOCKS_PER_DAY : value;
}

export function blockDate(block) {
  if (block < 0) return '—';
  const date = new Date(Date.UTC(2027, 0, 11) + Math.floor((block - 1) / BLOCKS_PER_DAY) * 86400000);
  if (Number.isNaN(date.getTime())) return `Block ${new Intl.NumberFormat('en').format(block)}`;
  return new Intl.DateTimeFormat('en', { month: 'short', day: 'numeric', timeZone: 'UTC' }).format(date);
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
  const value = unit === 'BTC' ? sats / SATS_PER_BTC : unit === 'USD' ? sats * (btcUsd / SATS_PER_BTC) : sats;
  const minimumFractionDigits = digits ?? (unit === 'BTC' ? 8 : unit === 'USD' ? 2 : 0);
  return new Intl.NumberFormat('en', { minimumFractionDigits, maximumFractionDigits: unit === 'USD' && isRate ? 6 : minimumFractionDigits, roundingMode: 'trunc' }).format(value);
}

export function parseBtcUsdQuote(quote) {
  const data = quote?.data;
  if (data?.base !== 'BTC' || data?.currency !== 'USD' || typeof data.amount !== 'string' || !data.amount.trim()) throw new Error('Invalid BTC/USD quote');
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
  return block >= contribution.start_block && block < contribution.start_block + contribution.duration ? contributionFee(contribution) : 0;
}

export function serviceIsActive(contributions, userId, block) {
  return contributions.some(contribution => contribution.mode === 'service' && contribution.user_id === userId && block >= contribution.start_block && block < contribution.start_block + contribution.duration);
}

export function mintingBurnAt(contribution, block, contributions) {
  if (contribution.mode === 'delegating' && !serviceIsActive(contributions, contribution.service_user_id, block)) return 0;
  return burnAt(contribution, block);
}

export function chartBlocks(contributions, firstBlock, lastBlock, heightNow, pending = null) {
  const count = lastBlock - firstBlock + 1;
  if (count <= 2001) return Array.from({ length: count }, (_, index) => firstBlock + index);
  // Rates only change at schedule boundaries; keep both sides so even one-block spikes survive zooming out.
  const blocks = new Set([firstBlock, lastBlock]);
  const add = block => { if (block >= firstBlock && block <= lastBlock) blocks.add(block); };
  add(heightNow);
  for (const fraction of [.25, .5, .75]) add(firstBlock + (lastBlock - firstBlock) * fraction);
  const schedule = contribution => {
    const burn = contribution.budget_sats - contribution.duration * contributionFee(contribution);
    const remainder = burn % contribution.duration;
    const boundaries = [contribution.start_block, contribution.start_block + contribution.duration];
    if (remainder > 0) boundaries.push(contribution.start_block + remainder);
    for (const block of boundaries) { add(block - 1); add(block); }
  };
  contributions.forEach(schedule);
  if (pending) schedule(pending);
  return [...blocks].sort((left, right) => left - right);
}

export function projectedReward(contributions, draft) {
  const start = draft.start_block;
  const end = start + draft.duration;
  const events = new Map();
  const eventAt = block => {
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
    const burn = BigInt(contribution.budget_sats) - duration * BigInt(contributionFee(contribution));
    const rate = burn / duration;
    const remainder = Number(burn % duration);
    add(contribution.start_block, rate + (remainder > 0 ? 1n : 0n), contribution, own);
    if (remainder > 0) add(contribution.start_block + remainder, -1n, contribution, own);
    add(contribution.start_block + contribution.duration, -rate, contribution, own);
    if (contribution.mode === 'service') {
      for (const [block, delta] of [[contribution.start_block, 1], [contribution.start_block + contribution.duration, -1]]) {
        const event = eventAt(block);
        event.services.set(contribution.user_id, (event.services.get(contribution.user_id) ?? 0) + delta);
      }
    }
  };
  schedule(draft, true);
  contributions.forEach(contribution => schedule(contribution));

  // Burn rates only change at schedule boundaries; keep sums exact before computing shares.
  let reward = 0;
  let previous = start;
  let total = 0n;
  let own = 0n;
  const delegated = new Map();
  const active = new Map();
  for (const [block, event] of [...events].sort(([left], [right]) => left - right)) {
    const available = draft.mode !== 'delegating' || (active.get(draft.service_user_id) ?? 0) > 0;
    if (total > 0n && available) reward += REWARD * Number(own) / Number(total) * (block - previous);
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
  const selfDelegating = Number.isSafeInteger(draft.user_id) && draft.user_id > 0 && draft.user_id === draft.service_user_id;
  return draft.mode === 'delegating' && !selfDelegating ? reward * (10000 - (draft.commission_bps ?? 0)) / 10000 : reward;
}

// Originals plus RGB midpoints, ordered for maximum minimum adjacent OKLab distance, including wrap.
export const PALETTE = ['#ffd800', '#ff6922', '#ffc200', '#ff4242', '#ff9c00', '#ffe400', '#ff9921', '#fff000', '#ffaf00', '#ff5632', '#ffcd00', '#ff8311'];
export const BLUE_PALETTE = ['#64b5ff', '#2354d8', '#409bff', '#173a9e', '#2478ed', '#83c7ff', '#3865ba', '#a3d9ff', '#3289f5', '#2b46ad', '#51a8ff', '#2d68db'];

export function userColor(userId, mode = 'independent') {
  const palette = mode === 'delegating' ? BLUE_PALETTE : PALETTE;
  return palette[userId % palette.length];
}

export function contributionStatus(contribution, height) {
  if (height < contribution.start_block) return 'scheduled';
  return height < contribution.start_block + contribution.duration - 1 ? 'active' : 'finished';
}

if (typeof document !== 'undefined') {
  const element = id => document.getElementById(id);
  const minting = location.pathname.replace(/\/$/, '') === '/mint';
  const number = (value, digits = 0) => new Intl.NumberFormat('en', { maximumFractionDigits: digits }).format(value);
  const whole = value => number(Math.trunc(value) || 0);
  let unit = 'sat';
  let timeMode = 'blocks';
  let zoom = ZOOM_DEFAULT;
  try {
    const saved = localStorage.getItem(UNIT_KEY);
    if (['sat', 'BTC', 'USD'].includes(saved)) unit = saved;
  } catch { /* Unit selection also works without browser storage. */ }
  try {
    const saved = localStorage.getItem(TIME_KEY);
    if (['blocks', 'days'].includes(saved)) timeMode = saved;
  } catch { /* Time selection also works without browser storage. */ }
  try {
    const saved = Number(localStorage.getItem(ZOOM_KEY));
    if (Number.isSafeInteger(saved) && saved >= ZOOM_MIN) zoom = saved;
  } catch { /* Zoom also works without browser storage. */ }
  let btcUsd = null;
  let quoteRequest = false;
  let quoteStatus = 'Loading BTC/USD…';
  const unitLabel = () => unit === 'USD' ? '$' : unit;
  const money = (sats, isRate = false) => formatMoney(sats, unit, btcUsd, isRate);
  const amount = sats => {
    const value = money(sats);
    return value === '—' ? value : unit === 'USD' ? `$${value}` : `${value} ${unit}`;
  };
  const rate = (sats, reward) => reward > 0 ? money(sats * SPARKS / reward, true) : '—';
  const rateAmount = (sats, reward) => {
    const value = rate(sats, reward);
    return value === '—' ? value : unit === 'USD' ? `$${value} / FLM` : `${value} ${unit} / FLM`;
  };
  const metricAmount = sats => {
    const value = sats === null ? '—' : formatMoney(sats, unit, btcUsd, false, unit === 'USD' ? 3 : null);
    return unit === 'USD' ? `$${value}` : `${value}\u00a0${unit.toLowerCase()}`;
  };
  const metricReward = sparks => `${sparks === null ? '—' : whole(sparks / SPARKS)}\u00a0FLM`;
  const metricRate = (sats, reward) => `${rate(sats, reward)}\u00a0${unitLabel()}/FLM`;
  const walletKey = 'flame-mint-playground-wallet-v1';
  const pendingKey = 'flame-mint-playground-pending-v1';
  let state = null;
  let wallet = null;
  let submitting = false;
  let pendingRequest = null;
  let brokenPending = false;
  let lastReceived = 0;
  let chartQueued = false;
  let feeFill = null;
  const chartContributions = () => state?.chart_contributions ?? state?.contributions ?? [];
  let minter = null;
  let minterBusy = false;
  let minterRequest = 0;
  let selectedMode = 'independent';
  let minterSort = 'name';
  let minterSortDescending = false;
  let serviceOptionsKey = '';
  let commissionDirty = false;
  let snapshotRequest = 0;

  element('nav-dashboard').setAttribute('aria-current', minting ? 'false' : 'page');
  element('nav-mint').setAttribute('aria-current', minting ? 'page' : 'false');

  for (const color of [...PALETTE, ...BLUE_PALETTE]) {
    const swatch = document.createElement('span');
    swatch.style.background = color;
    swatch.title = color;
    swatch.setAttribute('role', 'listitem');
    swatch.setAttribute('aria-label', color);
    element('palette-preview').append(swatch);
  }
  element('empty-table').textContent = 'No contributions.';
  if (minting) {
    document.title = 'Mint';
    element('page-title').textContent = 'Mint';
    element('minter-profile').hidden = false;
    element('total-rate-label').textContent = 'Effective rate';
    element('total-rate-label').title = 'Your burned coins divided by your FLM earned, excluding mining fees';
    element('mint-form').hidden = false;
    element('wallet-panel').hidden = false;
    element('own-legend').hidden = false;
    element('draft-legend').hidden = false;
    element('fees-legend').hidden = true;
    element('chart-wrap').className = 'canvas-wrap';
    element('chart-title').textContent = 'Burn rate';
    element('others-label').textContent = 'Others';
    element('contributions-title').textContent = 'Your contributions';
    try {
      const saved = JSON.parse(localStorage.getItem(pendingKey) || 'null');
      if (saved) {
        const legacyDuration = Number.isInteger(saved.duration) && saved.duration >= 100 && saved.duration <= 1000 && saved.duration % 50 === 0;
        const currentDuration = Number.isSafeInteger(saved.duration) && saved.duration >= DURATION_MIN;
        const delay = saved.delay_blocks === undefined ? 0 : saved.delay_blocks;
        if (typeof saved.address !== 'string' || typeof saved.request_id !== 'string' || !Number.isSafeInteger(saved.budget_sats) || saved.budget_sats < BUDGET_MIN || !(legacyDuration || currentDuration) || !Number.isSafeInteger(delay) || delay < 0) throw new Error('Invalid saved request');
        pendingRequest = { address: saved.address, budget_sats: saved.budget_sats, duration: saved.duration, delay_blocks: delay, request_id: saved.request_id };
        if (saved.mode !== undefined) {
          if (!['independent', 'service', 'delegating'].includes(saved.mode)) throw new Error('Invalid saved mode');
          pendingRequest.mode = saved.mode;
        }
        if (saved.service_user_id !== undefined) {
          if (!Number.isSafeInteger(saved.service_user_id) || saved.service_user_id < 1) throw new Error('Invalid saved service');
          pendingRequest.service_user_id = saved.service_user_id;
        }
        if ((saved.mode === 'delegating') !== (saved.service_user_id !== undefined) || (saved.mode === 'service' && saved.service_user_id !== undefined)) throw new Error('Invalid saved service');
        selectedMode = saved.mode ?? 'independent';
        element('budget').value = String(snapToRange(saved.budget_sats, BUDGET_MIN, BUDGET_MAX, BUDGET_STEP));
        element('duration').value = String(snapToRange(saved.duration, DURATION_MIN, DURATION_MAX));
        element('delay').value = String(snapToRange(delay, 0, DELAY_MAX));
        element('submit-message').textContent = 'Saved request unconfirmed. Retry safely with the same settings.';
      }
    } catch {
      brokenPending = true;
      element('submit-message').textContent = 'Saved request unreadable. Discard it before submitting again.';
    }
  }

  // Exact typed values are independent of the clamped slider positions.
  const typedValues = {
    budget: pendingRequest?.budget_sats ?? null,
    duration: pendingRequest ? Math.max(DURATION_MIN, pendingRequest.duration) : null,
    delay: pendingRequest?.delay_blocks ?? null,
  };
  const ownUserId = () => minter?.user_id ?? state?.minters?.find(account => account.address === wallet?.address)?.user_id ?? state?.contributions.find(contribution => contribution.address === wallet?.address)?.user_id;
  const serviceCommission = () => minter?.service_commission_bps ?? state?.services?.find(service => service.user_id === ownUserId())?.commission_bps ?? 200;
  const enteredCommission = () => {
    const text = element('commission').value.trim();
    const scaled = text ? Number(text) * 100 : NaN;
    return Math.abs(scaled - Math.round(scaled)) < 1e-7 ? Math.round(scaled) : NaN;
  };
  const needsCommissionSave = contribution => contribution.mode === 'service' && !pendingRequest && commissionDirty;
  const draft = () => {
    const delay = pendingRequest?.delay_blocks ?? typedValues.delay ?? Number(element('delay').value);
    const mode = pendingRequest?.mode ?? selectedMode;
    const serviceId = pendingRequest?.service_user_id ?? Number(element('service-select').value);
    const service = state?.services?.find(service => service.user_id === serviceId);
    const commissionBps = mode === 'delegating' ? service?.commission_bps : serviceCommission();
    return { budget_sats: pendingRequest?.budget_sats ?? typedValues.budget ?? Number(element('budget').value), duration: pendingRequest?.duration ?? typedValues.duration ?? Number(element('duration').value), delay_blocks: delay, start_block: (state?.height ?? 0) + 1 + delay, user_id: ownUserId(), mode, commission_bps: mode === 'independent' ? 0 : commissionBps, service_user_id: mode === 'delegating' ? serviceId : null };
  };
  const validDraft = contribution => [contribution.budget_sats, contribution.duration, contribution.delay_blocks, contribution.start_block].every(Number.isSafeInteger)
    && contribution.budget_sats >= BUDGET_MIN && contribution.duration >= (pendingRequest ? 100 : DURATION_MIN) && contribution.delay_blocks >= 0
    && contribution.start_block <= Number.MAX_SAFE_INTEGER - contribution.duration
    && contribution.budget_sats >= contribution.duration * (contributionFee(contribution) + 1)
    && (contribution.mode !== 'delegating' || !!pendingRequest || Number.isInteger(contribution.commission_bps))
    && (contribution.mode !== 'delegating' || (Number.isSafeInteger(contribution.service_user_id) && contribution.service_user_id > 0 && (!!pendingRequest || state?.services?.some(service => service.user_id === contribution.service_user_id))));
  const typedField = id => element(`${id}-value`);
  // Keep a field the user is editing untouched: the page re-renders every second.
  const showTyped = (id, text, unitText) => {
    element(`${id}-unit`).textContent = unitText;
    const field = typedField(id);
    if (typeof document !== 'undefined' && document.activeElement === field) return;
    field.value = text;
  };
  const sliderBounds = { budget: [BUDGET_MIN, BUDGET_MAX, BUDGET_STEP], duration: [DURATION_MIN, DURATION_MAX, 1], delay: [0, DELAY_MAX, 1] };
  function applyTyped(id) {
    const slider = element(id);
    const parsed = parseInputNumber(typedField(id).value);
    const native = id === 'budget' ? inputToSats(parsed, unit, btcUsd) : inputToBlocks(parsed, timeMode);
    const exact = snapToRange(native, sliderBounds[id][0], Number.MAX_SAFE_INTEGER);
    if (exact === null || slider.disabled) { updateDraft(); return; }
    typedValues[id] = exact;
    slider.value = String(snapToRange(exact, ...sliderBounds[id]));
    updateDraft(true);
  }

  function updateDraft(resetRequest = false) {
    if (resetRequest && !pendingRequest) element('submit-message').textContent = '';
    if (!commissionDirty) element('commission').value = String(serviceCommission() / 100);
    const contribution = draft();
    const valid = validDraft(contribution);
    const saveNeeded = needsCommissionSave(contribution);
    const fee = contributionFee(contribution);
    for (const mode of ['independent', 'service', 'delegating']) element(`mode-${mode}`).checked = contribution.mode === mode;
    element('service-options').hidden = contribution.mode !== 'service';
    element('delegation-options').hidden = contribution.mode !== 'delegating';
    element('fee-label').textContent = `Fee: ${amount(fee)}/block`;
    showTyped('budget', money(contribution.budget_sats), unit);
    showTyped('duration', formatDurationValue(contribution.duration, timeMode), durationUnit(contribution.duration, timeMode));
    showTyped('delay', formatDurationValue(contribution.delay_blocks, timeMode), durationUnit(contribution.delay_blocks, timeMode));
    element('budget').setAttribute('aria-valuetext', amount(Number(element('budget').value)));
    for (const id of ['duration', 'delay']) {
      const blocks = Number(element(id).value);
      element(id).setAttribute('aria-valuetext', `${formatDuration(blocks, timeMode)} (${blocks} blocks)`);
    }
    const selfDelegating = contribution.mode === 'delegating' && contribution.user_id === contribution.service_user_id;
    element('projection-note').textContent = `Known contribution schedules included. Starts ${timeMode === 'days' ? blockDate(contribution.start_block) : `at block ${number(contribution.start_block)}`}.${contribution.mode === 'delegating' ? ` No rewards or commission while the service is inactive; your budget still burns.${selfDelegating ? ' Your commission returns to you.' : ''}` : contribution.mode === 'service' ? ' Future delegation income is not estimated.' : ''}`;
    element('budget-error').textContent = saveNeeded ? 'Save commission before submitting.' : valid ? '' : contribution.mode === 'delegating' && !contribution.service_user_id ? 'Select a service.'
      : contribution.budget_sats < contribution.duration * (fee + 1)
      ? `Minimum ${amount(contribution.duration * (fee + 1))} (${amount(contribution.duration * fee)} fees + ${amount(1)} burned per block). Increase budget or shorten duration.`
      : 'Values and the end block must not exceed 9,007,199,254,740,991.';
    const canEstimate = state && wallet && valid && Number.isInteger(contribution.commission_bps);
    const estimate = canEstimate ? projectedReward(state.contributions, contribution) : 0;
    element('projected-reward').textContent = canEstimate ? whole(estimate / SPARKS) : '—';
    element('projected-fees').textContent = amount(contribution.duration * fee);
    element('projected-burn').textContent = valid ? amount(contribution.budget_sats - contribution.duration * fee) : '—';
    element('projected-rate').textContent = estimate ? rateAmount(contribution.budget_sats - contribution.duration * fee, estimate) : '—';
    const locked = submitting || !!pendingRequest || brokenPending;
    for (const id of ['budget', 'duration', 'delay']) element(id).disabled = typedField(id).disabled = locked;
    for (const id of ['mode-independent', 'mode-service', 'mode-delegating']) element(id).disabled = locked;
    element('commission').disabled = submitting || minterBusy || !wallet || !minter || contribution.mode !== 'service';
    const inputCommission = enteredCommission();
    element('save-commission').disabled = element('commission').disabled || !Number.isInteger(inputCommission) || inputCommission < 0 || inputCommission > 10000;
    element('service-select').disabled = locked || contribution.mode !== 'delegating';
    element('discard-pending').hidden = !pendingRequest && !brokenPending;
    element('discard-pending').disabled = submitting;
    element('submit').disabled = !valid || saveNeeded || !wallet || !state || submitting || brokenPending || Date.now() - lastReceived > 5000;
    element('submit').textContent = submitting ? 'Submitting…' : !wallet ? 'Wallet not ready' : pendingRequest ? 'Retry pending' : 'Submit';
    queueChart();
  }

  function resetMinter() {
    ++minterRequest;
    minter = null;
    minterBusy = false;
    commissionDirty = false;
    element('minter-name').value = '';
    element('minter-name').disabled = element('save-minter-name').disabled = true;
    element('minter-name-status').textContent = '';
    element('commission-status').textContent = '';
  }

  async function updateMinter(name, serviceCommissionBps) {
    if (!minting || !wallet || minterBusy) return;
    const changingCommission = serviceCommissionBps !== undefined;
    const field = element('minter-name');
    const status = element(changingCommission ? 'commission-status' : 'minter-name-status');
    if (changingCommission && (!Number.isInteger(serviceCommissionBps) || serviceCommissionBps < 0 || serviceCommissionBps > 10000)) {
      status.textContent = 'Use 0–100%, with at most two decimal places.';
      return;
    }
    if (name !== undefined) {
      name = name.trim();
      if (!name && !minter) name = undefined; // Retry a failed initial load without changing the default.
      else if (!name || [...name].length > 64 || /[\u0000-\u001f\u007f-\u009f]/.test(name)) {
        status.textContent = 'Use 1–64 characters without control characters.';
        return;
      }
    }
    const address = wallet.address;
    const request = ++minterRequest;
    minterBusy = true;
    field.disabled = element('save-minter-name').disabled = true;
    status.textContent = changingCommission || name !== undefined ? 'Saving…' : 'Loading…';
    updateDraft();
    try {
      const response = await fetch('/api/minter', { method: 'POST', headers: { 'Content-Type': 'application/json' }, signal: AbortSignal.timeout(10000), body: JSON.stringify({ address, ...(name === undefined ? {} : { name }), ...(changingCommission ? { service_commission_bps: serviceCommissionBps } : {}) }) });
      const result = await response.json();
      if (!response.ok) throw new Error(result.error || `Server returned ${response.status}`);
      if (!Number.isSafeInteger(result.user_id) || result.user_id < 1 || typeof result.name !== 'string' || !result.name.trim()) throw new Error('Invalid minter profile');
      if (result.service_commission_bps != null && (!Number.isInteger(result.service_commission_bps) || result.service_commission_bps < 0 || result.service_commission_bps > 10000)) throw new Error('Invalid service commission');
      if (wallet?.address !== address || minterRequest !== request) return;
      minter = { user_id: result.user_id, name: result.name, service_commission_bps: result.service_commission_bps ?? null };
      if (!changingCommission) field.value = result.name;
      if (changingCommission) {
        commissionDirty = false;
        element('commission').value = String(minter.service_commission_bps / 100);
        await poll(false);
      }
      if (wallet?.address === address && minterRequest === request) status.textContent = changingCommission ? 'Saved · applies from the next block.' : name === undefined ? '' : 'Saved';
    } catch (error) {
      if (wallet?.address === address && minterRequest === request) status.textContent = error.message || String(error);
    } finally {
      if (wallet?.address === address && minterRequest === request) {
        minterBusy = false;
        field.disabled = element('save-minter-name').disabled = false;
        updateDraft();
      }
    }
  }

  async function prepareWallet() {
    resetMinter();
    element('retry-wallet').hidden = true;
    element('reset-wallet').hidden = true;
    element('wallet-status').textContent = 'Wallet loading…';
    try {
      const stored = JSON.parse(localStorage.getItem(walletKey) || 'null');
      if (stored && (typeof stored.address !== 'string' || typeof stored.mnemonic !== 'string')) throw new Error('The saved wallet is unreadable');
      const load = async () => {
        const { default: init, generateMnemonic, Wallet } = await import('/wallet/flamewallet_wasm.js');
        await init();
        const mnemonic = stored ? stored.mnemonic : generateMnemonic(12);
        const keys = Wallet.fromMnemonic(mnemonic, '', 'testnet', 0);
        try {
          const address = keys.nextAddress().receivingAddress;
          if (stored && stored.address !== address) throw new Error('The saved address does not match the recovery phrase');
          return { mnemonic, address };
        } finally { keys.free(); }
      };
      let timeout;
      try {
        wallet = await Promise.race([load(), new Promise((_, reject) => {
          timeout = setTimeout(() => reject(new Error('The browser wallet did not finish loading.')), 15000);
        })]);
      } finally { clearTimeout(timeout); }
      // Saving must succeed before accepting contributions for this identity.
      localStorage.setItem(walletKey, JSON.stringify(wallet));
      element('wallet-address').textContent = wallet.address;
      element('wallet-status').textContent = 'Saved locally. Keys stay in this browser.';
    } catch (error) {
      wallet = null;
      element('wallet-status').textContent = `Wallet unavailable: ${error.message || String(error)}. Check that the wallet assets were built and browser storage is enabled.`;
      element('retry-wallet').hidden = false;
      element('reset-wallet').hidden = false;
    }
    updateDraft();
    if (state) renderTable();
    if (wallet) updateMinter();
  }

  function renderServices() {
    if (!minting) return;
    const key = JSON.stringify([state?.services ?? [], pendingRequest?.service_user_id]);
    if (key === serviceOptionsKey) return;
    serviceOptionsKey = key;
    const select = element('service-select');
    const selected = pendingRequest?.service_user_id ?? select.value;
    const options = document.createDocumentFragment();
    const placeholder = document.createElement('option');
    placeholder.value = '';
    placeholder.textContent = 'Select a service';
    options.append(placeholder);
    for (const service of state?.services ?? []) {
      const option = document.createElement('option');
      option.value = String(service.user_id);
      option.textContent = `${service.name} · ${service.commission_bps / 100}%${service.active === false ? ' · inactive' : ''}`;
      options.append(option);
    }
    if (pendingRequest?.service_user_id && !state?.services?.some(service => service.user_id === pendingRequest.service_user_id)) {
      const option = document.createElement('option');
      option.value = String(pendingRequest.service_user_id);
      option.textContent = `Service #${pendingRequest.service_user_id} (pending request)`;
      options.append(option);
    }
    select.replaceChildren(options);
    select.value = String(selected);
  }

  function renderMinters() {
    const minters = state?.minters ?? [];
    element('minters-panel').hidden = minting || minters.length === 0;
    if (minting) return;
    const sorted = minters.slice().sort((left, right) => {
      let comparison;
      if (minterSort === 'name') comparison = left.name.localeCompare(right.name);
      else if (minterSort === 'rate') {
        const leftReward = BigInt(left.reward_sparks), rightReward = BigInt(right.reward_sparks);
        if (leftReward === 0n || rightReward === 0n) return leftReward === rightReward ? left.user_id - right.user_id : leftReward === 0n ? 1 : -1;
        const difference = BigInt(left.burned_sats) * rightReward - BigInt(right.burned_sats) * leftReward;
        comparison = difference > 0n ? 1 : difference < 0n ? -1 : 0;
      } else {
        const difference = BigInt(left[minterSort]) - BigInt(right[minterSort]);
        comparison = difference > 0n ? 1 : difference < 0n ? -1 : 0;
      }
      return (minterSortDescending ? -comparison : comparison) || left.user_id - right.user_id;
    });
    const rows = document.createDocumentFragment();
    for (const minter of sorted) {
      const row = document.createElement('tr');
      const identity = document.createElement('td');
      const heading = document.createElement('div');
      heading.className = 'contribution-identity';
      const dot = document.createElement('i');
      dot.className = 'participant-dot';
      dot.style.background = userColor(minter.user_id, minter.mode);
      const name = document.createElement('span');
      name.textContent = `${minter.name}${minter.mode === 'service' ? ` · ${minter.commission_bps / 100}%` : ''}`;
      heading.append(dot, name);
      const mode = document.createElement('p');
      mode.className = 'address-label';
      mode.textContent = minter.mode === 'service' ? 'Service' : minter.mode === 'delegating' ? 'Delegating' : 'Independent';
      identity.append(heading, mode);
      row.append(identity);
      for (const value of [money(Number(minter.burned_sats)), whole(Number(minter.reward_sparks) / SPARKS), money(Number(minter.fees_sats)), rate(Number(minter.burned_sats), Number(minter.reward_sparks))]) {
        const cell = document.createElement('td'); cell.className = 'number'; cell.textContent = value; row.append(cell);
      }
      rows.append(row);
    }
    element('minters').replaceChildren(rows);
    for (const key of ['name', 'burned_sats', 'reward_sparks', 'fees_sats', 'rate']) {
      element(`sort-${key}`).parentElement?.setAttribute('aria-sort', key === minterSort ? minterSortDescending ? 'descending' : 'ascending' : 'none');
    }
  }

  function renderTable() {
    const account = state.minters?.find(minter => minter.address === wallet?.address);
    const ownUserId = minter?.user_id ?? account?.user_id ?? state.contributions.find(contribution => contribution.address === wallet?.address)?.user_id;
    const contributions = state.contributions.filter(contribution => !minting || contribution.address === wallet?.address || (wallet && ownUserId !== undefined && contribution.mode === 'delegating' && contribution.service_user_id === ownUserId)).slice().sort((a, b) => b.id - a.id);
    const rows = document.createDocumentFragment();
    let burned = 0, fees = 0, rewards = 0;
    for (const contribution of contributions) {
      const row = document.createElement('tr');
      const identity = document.createElement('td');
      const heading = document.createElement('div');
      heading.className = 'contribution-identity';
      const dot = document.createElement('i');
      dot.className = 'participant-dot';
      dot.style.background = userColor(contribution.user_id, contribution.mode);
      const title = document.createElement('span');
      title.className = 'contribution-id';
      title.textContent = `${contribution.user_name ?? `Minter ${contribution.user_id}`} #${contribution.id}`;
      heading.append(dot, title);
      const address = document.createElement('p');
      address.className = 'address-label';
      const end = contribution.start_block + contribution.duration - 1;
      address.textContent = `${amount(contribution.budget_sats)} / ${formatDuration(contribution.duration, timeMode)}`;
      address.title = minting ? contribution.address : '';
      identity.append(heading, address);
      row.append(identity);
      const minterCell = document.createElement('td');
      minterCell.textContent = contribution.minter_name ?? contribution.user_name ?? `Minter ${contribution.user_id}`;
      row.append(minterCell);
      const statusCell = document.createElement('td');
      const status = document.createElement('span');
      const statusName = contributionStatus(contribution, state.height);
      status.className = `status ${statusName}`;
      const when = block => timeMode === 'days' ? blockDate(block) : `block ${number(block)}`;
      const totalBurn = contribution.budget_sats - contribution.duration * contributionFee(contribution);
      const progress = Math.max(0, Math.min(100, Number(BigInt(contribution.burned_sats) * 100n / BigInt(totalBurn))));
      status.textContent = statusName === 'scheduled' ? `Scheduled at ${when(contribution.start_block)}` : statusName === 'finished' ? `Finished on ${when(end)}` : `Active ${progress}%`;
      statusCell.append(status);
      row.append(statusCell);
      const own = !minting || contribution.address === wallet?.address;
      const linked = minting && wallet && ownUserId !== undefined && contribution.mode === 'delegating' && contribution.service_user_id === ownUserId;
      const burn = own ? Number(contribution.burned_sats) : 0, fee = own ? Number(contribution.fees_sats) : 0;
      const reward = (own ? Number(contribution.reward_sparks) : 0) + (linked ? Number(contribution.commission_sparks ?? 0) : 0);
      for (const value of [money(burn), whole(reward / SPARKS), money(fee), rate(burn, reward)]) {
        const cell = document.createElement('td'); cell.className = 'number'; cell.textContent = value; row.append(cell);
      }
      burned += burn; fees += fee; rewards += reward;
      rows.append(row);
    }
    element('contributions').replaceChildren(rows);
    element('empty-table').hidden = contributions.length > 0;
    if (!minting) {
      burned = Number(state.totals.burned_sats);
      fees = Number(state.totals.fees_sats);
      rewards = Number(state.totals.reward_sparks);
    } else if (account) {
      burned = Number(account.burned_sats);
      fees = Number(account.fees_sats);
      rewards = Number(account.reward_sparks);
    }
    const totalRow = document.createElement('tr');
    const count = minting ? contributions.length : state.totals.contribution_count;
    for (const [index, value] of [minting ? 'Total' : 'All-time total', '', `${count} contributions`, money(burned), whole(rewards / SPARKS), money(fees), rate(burned, rewards)].entries()) {
      const cell = document.createElement('td'); if (index > 2) cell.className = 'number'; cell.textContent = value; totalRow.append(cell);
    }
    element('contribution-totals').replaceChildren(totalRow);
    if (minting) {
      element('total-burned').textContent = metricAmount(wallet ? burned : null);
      element('total-fees').textContent = metricAmount(wallet ? fees : null);
      element('total-reward').textContent = metricReward(wallet ? rewards : null);
      element('total-rate').textContent = metricRate(burned, wallet ? rewards : 0);
    }
  }

  function renderMoney() {
    const label = unitLabel();
    for (const value of ['sat', 'BTC', 'USD']) element(`unit-${value.toLowerCase()}`).setAttribute('aria-pressed', String(unit === value));
    element('price-status').textContent = quoteStatus;
    element('table-burned-label').textContent = unit === 'sat' ? 'Sats burned' : `${label} burned`;
    element('table-fees-label').textContent = `Mining fees (${label})`;
    element('table-rate-label').textContent = `${label} / FLM`;
    element('chart-burn-unit').textContent = `${label}/block`;
    element('chart-rate-unit').textContent = `${label}/FLM`;
    const spokenUnit = unit === 'sat' ? 'satoshis' : unit === 'USD' ? 'US dollars' : 'BTC';
    element('chart').setAttribute('aria-label', minting
      ? `Burn rate in ${spokenUnit} per block on the left; mint rate in ${spokenUnit} per FLM on the right. Previous and next ${zoom} blocks, shown as ${timeMode === 'days' ? 'calendar dates' : 'block numbers'}. All known contribution schedules included.`
      : `Burn above zero and mining fees below zero share a ${spokenUnit}-per-block scale on the left. Right-hand values are divided by 50 FLM. Previous and next ${zoom} blocks, shown as ${timeMode === 'days' ? 'calendar dates' : 'block numbers'}. All known contribution schedules included.`);
    element('zoom-in').disabled = zoom === ZOOM_MIN;
    element('zoom-out').disabled = zoom === Number.MAX_SAFE_INTEGER;
    for (const value of ['blocks', 'days']) element(`time-${value}`).setAttribute('aria-pressed', String(timeMode === value));
    element('current-block-label').textContent = timeMode === 'days' ? 'Date' : 'Block';
    element('current-block').textContent = state ? timeMode === 'days' ? blockDate(state.height) : number(state.height) : '—';
    element('chart-window').textContent = `±${formatDuration(zoom, timeMode)}`;
    element('duration-min').textContent = formatDuration(DURATION_MIN, timeMode);
    element('duration-max').textContent = formatDuration(DURATION_MAX, timeMode);
    element('delay-min').textContent = formatDuration(0, timeMode);
    element('delay-max').textContent = formatDuration(DELAY_MAX, timeMode);
    element('budget-min').textContent = amount(BUDGET_MIN);
    element('budget-max').textContent = amount(BUDGET_MAX);
    element('projected-rate-label').textContent = `${label}/FLM`;
    element('sort-rate').textContent = `${label} / FLM`;
    if (state) {
      renderServices();
      renderMinters();
      renderTable();
      if (!minting) {
        element('total-burned').textContent = metricAmount(Number(state.totals.burned_sats));
        element('total-fees').textContent = metricAmount(Number(state.totals.fees_sats));
        element('total-reward').textContent = metricReward(Number(state.totals.reward_sparks));
        const currentBurn = chartContributions().reduce((sum, contribution) => sum + burnAt(contribution, state.height), 0);
        const eligible = chartContributions().some(contribution => mintingBurnAt(contribution, state.height, chartContributions()) > 0);
        element('total-rate').textContent = metricRate(currentBurn, eligible ? REWARD : 0);
      }
    } else {
      element('total-burned').textContent = metricAmount(null);
      element('total-fees').textContent = metricAmount(null);
      element('total-reward').textContent = metricReward(null);
      element('total-rate').textContent = metricRate(0, 0);
    }
    updateDraft();
  }

  async function refreshQuote() {
    if (quoteRequest) return;
    quoteRequest = true;
    if (btcUsd === null) {
      quoteStatus = 'Loading BTC/USD…';
      renderMoney();
    }
    try {
      const response = await fetch('https://api.coinbase.com/v2/prices/BTC-USD/spot', { credentials: 'omit', referrerPolicy: 'no-referrer', cache: 'no-store', signal: AbortSignal.timeout(10000) });
      if (!response.ok) throw new Error(`Price service returned ${response.status}`);
      btcUsd = parseBtcUsdQuote(await response.json());
      quoteStatus = `BTC/USD $${formatMoney(SATS_PER_BTC, 'USD', btcUsd)} · Coinbase`;
    } catch {
      btcUsd = null;
      quoteStatus = 'BTC/USD unavailable. Retrying in 60 seconds.';
    } finally {
      quoteRequest = false;
      renderMoney();
      setTimeout(refreshQuote, 60000);
    }
  }

  function selectUnit(value) {
    unit = value;
    try { localStorage.setItem(UNIT_KEY, value); } catch { /* Unit selection still applies in this page. */ }
    renderMoney();
  }

  function selectTime(value) {
    timeMode = value;
    try { localStorage.setItem(TIME_KEY, value); } catch { /* Time selection still applies in this page. */ }
    renderMoney();
  }

  function changeZoom(direction) {
    const next = Math.max(ZOOM_MIN, Math.min(Number.MAX_SAFE_INTEGER, direction < 0 ? Math.round(zoom / 2) : zoom * 2));
    if (next === zoom) return;
    zoom = next;
    try { localStorage.setItem(ZOOM_KEY, String(zoom)); } catch { /* Zoom still applies in this page. */ }
    renderMoney();
  }

  function queueChart() {
    if (chartQueued) return;
    chartQueued = true;
    requestAnimationFrame(() => { chartQueued = false; drawChart(); });
  }

  function drawChart() {
    const canvas = element('chart');
    const width = canvas.clientWidth, height = canvas.clientHeight;
    if (!width || !height) return;
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.round(width * ratio); canvas.height = Math.round(height * ratio);
    const context = canvas.getContext('2d');
    context.scale(ratio, ratio);
    const side = unit === 'sat' ? 48 : unit === 'BTC' ? 94 : 82;
    const margin = { left: side, top: 14, right: side, bottom: 27 };
    const w = width - margin.left - margin.right, h = height - margin.top - margin.bottom;
    const heightNow = state?.height ?? 0, firstBlock = heightNow - zoom, lastBlock = heightNow + zoom;
    const pending = draft();
    const showDraft = minting && wallet && validDraft(pending);
    const contributions = chartContributions();
    const blocks = chartBlocks(contributions, firstBlock, lastBlock, heightNow, showDraft ? pending : null);
    const count = blocks.length;
    const colored = !minting;
    const groups = new Map();
    const colors = new Map();
    const ownKeys = new Set();
    const fees = new Float64Array(count);
    const ownContribution = minting && contributions.find(contribution => contribution.address === wallet?.address);
    const ownColor = ownContribution ? userColor(ownContribution.user_id, pending.mode) : '#888';
    if (minting) {
      element('own-swatch').style.background = ownContribution ? userColor(ownContribution.user_id, ownContribution.mode) : '#888';
      element('draft-swatch').style.borderColor = ownColor;
    }
    const ordered = minting ? contributions : contributions.slice().sort((a, b) => a.id - b.id);
    for (const contribution of ordered) {
      const own = minting && contribution.address === wallet?.address;
      const delegating = contribution.mode === 'delegating';
      const key = own ? delegating ? 'own-delegating' : 'own' : delegating ? `delegating:${contribution.user_id}` : colored ? contribution.user_id : 'other';
      colors.set(key, key === 'other' ? '#a3a3a3' : userColor(contribution.user_id, contribution.mode));
      const linked = minting && minter && delegating && contribution.service_user_id === minter.user_id;
      if (own || linked) ownKeys.add(key);
      const values = groups.get(key) || new Float64Array(count);
      // Move the whole user band to the top on each newer contribution.
      if (colored) groups.delete(key);
      groups.set(key, values);
      for (let i = 0; i < count; i++) {
        const block = blocks[i];
        values[i] += burnAt(contribution, block);
        if (!minting) fees[i] += feeAt(contribution, block);
      }
    }
    for (const key of ownKeys) {
      const values = groups.get(key);
      groups.delete(key);
      groups.set(key, values);
    }
    const totals = new Float64Array(count);
    for (const values of groups.values()) for (let i = 0; i < count; i++) totals[i] += values[i];
    let maximum = 0, maximumFees = 0;
    for (let i = 0; i < count; i++) {
      maximum = Math.max(maximum, totals[i] + (showDraft ? burnAt(pending, blocks[i]) : 0));
      maximumFees = Math.max(maximumFees, fees[i]);
    }
    const minimum = minting ? 0 : -maximumFees;
    const roughStep = Math.max(1, maximum - minimum) / 5;
    const power = 10 ** Math.floor(Math.log10(roughStep));
    const step = Math.max(1, [1, 2, 5, 10].find(factor => factor * power >= roughStep) * power);
    const high = Math.max(step, Math.ceil(maximum / step) * step);
    const low = Math.floor(minimum / step) * step;
    const xBlock = block => margin.left + (block - firstBlock) / (lastBlock - firstBlock) * w;
    const x = i => xBlock(blocks[i]);
    const nowX = xBlock(heightNow);
    const y = value => margin.top + (high - value) / (high - low) * h;
    const zero = y(0);
    const axisNumber = value => unit !== 'sat' ? money(value) : Math.abs(value) >= 1000000 ? `${number(value / 1000000, 2)}m` : Math.abs(value) >= 1000 ? `${number(value / 1000, 2)}k` : number(value, 2);
    context.font = '10px -apple-system, sans-serif';
    context.textBaseline = 'middle';
    for (let tick = Math.round(low / step); tick <= Math.round(high / step); tick++) {
      const value = tick * step;
      context.strokeStyle = '#e5e5e5'; context.lineWidth = 1;
      context.beginPath(); context.moveTo(margin.left, y(value)); context.lineTo(margin.left + w, y(value)); context.stroke();
      context.fillStyle = '#666'; context.textAlign = 'right';
      context.fillText(axisNumber(value), margin.left - 9, y(value));
      context.textAlign = 'left';
      context.fillText(money(value / (REWARD / SPARKS), true), width - margin.right + 8, y(value));
    }
    if (!minting) {
      if (!feeFill) {
        const tile = document.createElement('canvas');
        tile.width = tile.height = 8;
        const hatch = tile.getContext('2d');
        hatch.fillStyle = '#f2f2f2'; hatch.fillRect(0, 0, 8, 8);
        hatch.strokeStyle = '#888';
        hatch.beginPath(); hatch.moveTo(0, 8); hatch.lineTo(8, 0); hatch.stroke();
        feeFill = context.createPattern(tile, 'repeat');
      }
      context.beginPath(); context.moveTo(x(0), zero);
      for (let i = 0; i < count; i++) context.lineTo(x(i), y(-fees[i]));
      context.lineTo(x(count - 1), zero); context.closePath();
      context.fillStyle = feeFill; context.fill();
    }
    const base = new Float64Array(count);
    for (const [key, values] of groups) {
      context.beginPath(); context.moveTo(x(0), y(base[0]));
      for (let i = 0; i < count; i++) context.lineTo(x(i), y(base[i] + values[i]));
      for (let i = count - 1; i >= 0; i--) context.lineTo(x(i), y(base[i]));
      context.closePath(); context.fillStyle = colors.get(key); context.globalAlpha = key === 'other' ? .76 : 1; context.fill();
      for (let i = 0; i < count; i++) base[i] += values[i];
    }
    context.globalAlpha = 1;
    context.fillStyle = '#ffffff30'; context.fillRect(nowX, margin.top, w / 2, h);
    if (!minting) {
      context.strokeStyle = '#666';
      context.beginPath(); context.moveTo(x(0), zero); context.lineTo(x(count - 1), zero); context.stroke();
    }
    context.strokeStyle = '#666'; context.setLineDash([3, 4]); context.beginPath(); context.moveTo(nowX, margin.top); context.lineTo(nowX, margin.top + h); context.stroke(); context.setLineDash([]);
    if (showDraft) {
      const start = Math.max(firstBlock, pending.start_block);
      const end = Math.min(lastBlock, pending.start_block + pending.duration - 1);
      if (start <= end) {
        const from = Math.max(firstBlock, start - 1), to = Math.min(lastBlock, end + 1);
        let started = false;
        context.beginPath();
        for (let i = 0; i < count; i++) {
          if (blocks[i] < from || blocks[i] > to) continue;
          const value = totals[i] + burnAt(pending, blocks[i]);
          if (!started) context.moveTo(x(i), y(value)); else context.lineTo(x(i), y(value));
          started = true;
        }
        context.strokeStyle = ownColor; context.lineWidth = 1.8; context.setLineDash([5, 4]); context.stroke(); context.setLineDash([]);
      }
    }
    context.fillStyle = '#666'; context.font = '10px -apple-system, sans-serif';
    for (const fraction of [0, .25, .5, .75, 1]) {
      context.textAlign = fraction === 0 ? 'left' : fraction === 1 ? 'right' : 'center';
      const block = firstBlock + (lastBlock - firstBlock) * fraction;
      context.fillText(block < 0 ? '—' : timeMode === 'days' ? blockDate(block) : number(block), xBlock(block), height - 8);
    }
    if (!state) { context.textAlign = 'center'; context.fillText('Loading…', margin.left + w / 2, margin.top + h / 2); }
  }

  async function poll(reschedule = true) {
    const request = ++snapshotRequest;
    try {
      const response = await fetch(minting ? '/api/state?full_history=true' : '/api/state', { cache: 'no-store', signal: AbortSignal.timeout(10000) });
      if (!response.ok) throw new Error(`Server returned ${response.status}`);
      const snapshot = await response.json();
      if (request !== snapshotRequest) return;
      state = snapshot;
      if (minter) minter.service_commission_bps = state.services?.find(service => service.user_id === minter.user_id)?.commission_bps ?? null;
      lastReceived = Date.now();
      element('connection').className = 'connection';
      element('connection').textContent = 'Live';
      renderMoney();
    } catch (error) {
      if (request !== snapshotRequest) return;
      element('connection').className = 'connection error';
      element('connection').textContent = `Disconnected${state ? ' (last snapshot)' : ''}. ${error.message}. Retrying…`;
      if (minting) element('submit').disabled = true;
    } finally { if (reschedule) setTimeout(poll, 1000); }
  }

  for (const value of ['sat', 'BTC', 'USD']) element(`unit-${value.toLowerCase()}`).addEventListener('click', () => selectUnit(value));
  for (const value of ['blocks', 'days']) element(`time-${value}`).addEventListener('click', () => selectTime(value));
  for (const mode of ['independent', 'service', 'delegating']) element(`mode-${mode}`).addEventListener('change', () => {
    if (pendingRequest || submitting || brokenPending) return;
    selectedMode = mode;
    updateDraft(true);
  });
  for (const event of ['input', 'change']) element('commission').addEventListener(event, () => {
    commissionDirty = enteredCommission() !== serviceCommission();
    element('commission-status').textContent = '';
    updateDraft(true);
  });
  for (const event of ['input', 'change']) element('service-select').addEventListener(event, () => updateDraft(true));
  element('save-commission').addEventListener('click', () => updateMinter(undefined, enteredCommission()));
  element('commission-profile').addEventListener('submit', async event => {
    event.preventDefault();
    await updateMinter(undefined, enteredCommission());
  });
  for (const key of ['name', 'burned_sats', 'reward_sparks', 'fees_sats', 'rate']) element(`sort-${key}`).addEventListener('click', () => {
    minterSortDescending = minterSort === key ? !minterSortDescending : key !== 'name';
    minterSort = key;
    renderMinters();
  });
  element('zoom-in').addEventListener('click', () => changeZoom(-1));
  element('zoom-out').addEventListener('click', () => changeZoom(1));
  for (const id of ['budget', 'duration', 'delay']) {
    element(id).addEventListener('input', () => { typedValues[id] = null; updateDraft(true); });
    const field = typedField(id);
    field.addEventListener('change', () => applyTyped(id));
    // Enter applies the value instead of submitting the form.
    field.addEventListener('keydown', event => {
      if (event.key !== 'Enter') return;
      event.preventDefault();
      applyTyped(id);
      field.blur?.();
    });
    field.addEventListener('focus', () => field.select?.());
  }
  element('retry-wallet').addEventListener('click', prepareWallet);
  element('reset-wallet').addEventListener('click', () => {
    if (!confirm('Forget this saved demo wallet and create a new one? Save its recovery phrase first if it is available. Existing rewards will remain linked to the old address.')) return;
    try { localStorage.removeItem(walletKey); prepareWallet(); }
    catch (error) { element('wallet-status').textContent = error.message; }
  });
  element('discard-pending').addEventListener('click', () => {
    if (submitting || !confirm('Discard this pending request? It might already have been accepted. Discarding does not cancel it, and submitting again could create a second contribution. Retry instead if you are unsure.')) return;
    try {
      localStorage.removeItem(pendingKey); pendingRequest = null; brokenPending = false;
      element('submit-message').textContent = 'Pending request discarded. Check your contributions before submitting again.';
      updateDraft();
    } catch (error) { element('submit-message').textContent = error.message; }
  });
  element('seed-details').addEventListener('toggle', () => {
    element('seed-phrase').textContent = element('seed-details').open ? wallet?.mnemonic || 'Wallet is not available.' : '';
  });
  element('minter-profile').addEventListener('submit', async event => {
    event.preventDefault();
    await updateMinter(element('minter-name').value);
  });
  element('minter-name').addEventListener('input', () => { element('minter-name-status').textContent = ''; });
  element('contribution-form').addEventListener('submit', async event => {
    event.preventDefault();
    if (submitting || brokenPending || !wallet || !state || !validDraft(draft()) || needsCommissionSave(draft()) || Date.now() - lastReceived > 5000) return;
    submitting = true; updateDraft();
    for (const id of ['budget', 'duration', 'delay']) element(id).disabled = typedField(id).disabled = true;
    element('submit-message').className = 'form-message';
    element('submit-message').textContent = '';
    try {
      if (!pendingRequest) {
        const contribution = draft();
        const request = { address: wallet.address, budget_sats: contribution.budget_sats, duration: contribution.duration, delay_blocks: contribution.delay_blocks, request_id: crypto.randomUUID() };
        if (contribution.mode !== 'independent') request.mode = contribution.mode;
        if (contribution.mode === 'delegating') request.service_user_id = contribution.service_user_id;
        localStorage.setItem(pendingKey, JSON.stringify(request));
        pendingRequest = request;
      }
      const response = await fetch('/api/contributions', { method: 'POST', headers: { 'Content-Type': 'application/json' }, signal: AbortSignal.timeout(10000), body: JSON.stringify(pendingRequest) });
      const result = await response.json();
      if (!response.ok) throw new Error(result.error || `Server returned ${response.status}`);
      element('submit-message').className = 'form-message success';
      element('submit-message').textContent = `#${result.id} accepted. Starts ${timeMode === 'days' ? blockDate(result.start_block) : `at block ${number(result.start_block)}`}.`;
      localStorage.removeItem(pendingKey);
      pendingRequest = null;
    } catch (error) {
      element('submit-message').className = 'form-message';
      element('submit-message').textContent = `${error.message}. Request saved locally; retry safely after reloading.`;
    } finally {
      submitting = false; updateDraft();
    }
  });
  new ResizeObserver(queueChart).observe(element('chart'));
  renderMoney();
  refreshQuote();
  if (minting) prepareWallet();
  poll();
}
