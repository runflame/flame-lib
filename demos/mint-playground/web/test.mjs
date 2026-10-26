import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const source = await readFile(new URL('./app.js', import.meta.url), 'utf8');
const { BUDGET_MIN, BUDGET_MAX, DURATION_MIN, DURATION_MAX, DELAY_MAX, BLOCKS_PER_DAY, TIME_KEY, ZOOM_KEY, ZOOM_MIN, ZOOM_DEFAULT, chartBlocks, blockDate, burnAt, feeAt, contributionFee, projectedReward, PALETTE, userColor, contributionStatus, FEE, SPARKS, UNIT_KEY, formatMoney, parseBtcUsdQuote, BUDGET_STEP, parseInputNumber, snapToRange, inputToSats, inputToBlocks } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
const DEFAULT_ZOOM = 500;
const BLUE_PALETTE = ['#64b5ff', '#2354d8', '#409bff', '#173a9e', '#2478ed', '#83c7ff', '#3865ba', '#a3d9ff', '#3289f5', '#2b46ad', '#51a8ff', '#2d68db'];
const plan = (options = {}) => ({ user_id: 1, budget_sats: 300005, duration: 100, start_block: 10, burned_sats: '0', fees_sats: '0', reward_sparks: '0', commission_sparks: '0', ...options });

test('budget is a native linear slider with the requested endpoints', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  const slider = html.match(/<input\b[^>]*\bid="budget"[^>]*>/)[0];
  const attribute = name => Number(slider.match(new RegExp(`\\b${name}="([^"]+)"`))[1]);
  assert.equal(BUDGET_MIN, 100000);
  assert.equal(BUDGET_MAX, 100000000);
  assert.equal(attribute('min'), BUDGET_MIN);
  assert.equal(attribute('max'), BUDGET_MAX);
  assert.equal(attribute('step'), 100000);
  assert.equal(attribute('value'), 1000000);
});

test('duration and delay sliders use exact integer block bounds at single-block resolution', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  assert.equal(BLOCKS_PER_DAY, 144);
  assert.equal(DURATION_MIN, 144);
  assert.equal(DURATION_MAX, 4464);
  assert.equal(DELAY_MAX, 4464);
  for (const [id, min, max, value] of [['duration', 144, 4464, 500], ['delay', 0, 4464, 0]]) {
    const slider = html.match(new RegExp(`<input\\b[^>]*\\bid="${id}"[^>]*>`))[0];
    const attribute = name => Number(slider.match(new RegExp(`\\b${name}="([^"]+)"`))[1]);
    assert.equal(attribute('min'), min);
    assert.equal(attribute('max'), max);
    assert.equal(attribute('step'), 1);
    assert.equal(attribute('value'), value);
  }
});

test('calendar dates start at block one and use exact 144-block UTC days, including leap and year boundaries', () => {
  assert.equal(blockDate(0), 'Jan 10');
  for (const block of [1, 2, 143, 144]) assert.equal(blockDate(block), 'Jan 11');
  assert.equal(blockDate(145), 'Jan 12');
  assert.equal(blockDate(61 * 144 + 1), 'Mar 13');
  assert.equal(blockDate(355 * 144), 'Dec 31');
  assert.equal(blockDate(355 * 144 + 1), 'Jan 1');
  assert.equal(blockDate(414 * 144 + 1), 'Feb 29');
  assert.equal(blockDate(415 * 144 + 1), 'Mar 1');
  assert.equal(blockDate(-1), '—');
  assert.equal(blockDate(Number.MAX_SAFE_INTEGER), `Block ${new Intl.NumberFormat('en').format(Number.MAX_SAFE_INTEGER)}`, 'far-future manual schedules remain renderable beyond the Date range');
});

test('exponential zoom has a ten-block minimum and a fresh five-hundred-block default', () => {
  assert.equal(ZOOM_KEY, 'flame-mint-playground-zoom-v2');
  assert.equal(ZOOM_MIN, 10);
  assert.equal(ZOOM_DEFAULT, DEFAULT_ZOOM);
  assert.doesNotMatch(source, /ZOOM_LEVELS/);
});

test('integer schedule spreads the remainder into the first blocks and conserves sats', () => {
  const contribution = plan();
  assert.equal(burnAt(contribution, 9), 0);
  assert.equal(burnAt(contribution, 10), 2001);
  assert.equal(burnAt(contribution, 14), 2001);
  assert.equal(burnAt(contribution, 15), 2000);
  assert.equal(burnAt(contribution, 109), 2000);
  assert.equal(burnAt(contribution, 110), 0);
  let burn = 0;
  for (let block = 0; block < 200; block++) burn += burnAt(contribution, block);
  assert.equal(burn + contribution.duration * FEE, contribution.budget_sats);
});

test('delegating contributions burn their full budget and pay no per-block mining fee', () => {
  for (const mode of [undefined, 'independent', 'service', 'delegating']) {
    const contribution = plan({ mode, budget_sats: 300005 });
    const fee = mode === 'delegating' ? 0 : FEE;
    assert.equal(contributionFee(contribution), fee);
    let total = 0;
    for (let block = contribution.start_block; block < contribution.start_block + contribution.duration; block++) {
      total += burnAt(contribution, block) + feeAt(contribution, block);
      assert.equal(feeAt(contribution, block), fee);
    }
    assert.equal(total, contribution.budget_sats);
    assert.equal(burnAt(contribution, 10), mode === 'delegating' ? 3001 : 2001);
    assert.equal(feeAt(contribution, 9), 0);
    assert.equal(feeAt(contribution, 110), 0);
  }
});

test('delegated forecasts deduct commission from FLM, not from the burned budget', () => {
  const service = plan({ mode: 'service', user_id: 2, budget_sats: 300000, start_block: 11, commission_bps: 200 });
  for (const commission_bps of [0, 200, 1250, 10000]) {
    const draft = plan({ mode: 'delegating', service_user_id: 2, budget_sats: 300000, start_block: 11, commission_bps });
    const expected = 3000 * SPARKS * (10000 - commission_bps) / 10000;
    assert.equal(projectedReward([service], draft), expected);
    assert.equal(projectedReward([], draft), 0, 'burning without an active provider earns no FLM');
    assert.equal(burnAt(draft, 11), 3000, 'commission never reduces the underlying burned satoshis');
  }
  assert.equal(projectedReward([], service), 5000 * SPARKS, 'service forecasts do not speculate about future commission income');
});

test('delegated forecasts pause on provider expiry, resume on later service windows, and retain all burned costs', () => {
  const draft = plan({ mode: 'delegating', service_user_id: 2, commission_bps: 200, budget_sats: 200000, start_block: 11 });
  const first = plan({ mode: 'service', user_id: 2, budget_sats: 75000, duration: 25, start_block: 11 });
  const later = { ...first, start_block: 61 };
  assert.equal(projectedReward([first], draft), 25 * 25 * SPARKS * .98);
  assert.equal(projectedReward([first, later], draft), 50 * 25 * SPARKS * .98);
  assert.equal(projectedReward([{ ...first, mode: 'independent' }], draft), 0, 'independent activity by the same account is not a Service endorsement');
  assert.equal(projectedReward([{ ...first, user_id: 3 }], draft), 0, 'a different active Service cannot mint for the selected provider');
  assert.equal(Array.from({ length: 100 }, (_, i) => burnAt(draft, 11 + i)).reduce((sum, value) => sum + value), 200000, 'inactive windows still consume the full delegation budget');
  const independent = plan({ start_block: 11, budget_sats: 300000 });
  assert.equal(projectedReward([draft], independent), 5000 * SPARKS, 'orphaned delegated burn does not dilute eligible rewards');
});

test('mining fees count concurrent contributions, including the same address, not their budgets', () => {
  const contributions = [plan({ address: 'same-address', budget_sats: 100100 }), plan({ address: 'same-address', budget_sats: 100000000, start_block: 50 })];
  for (const [block, expected] of [[9, 0], [10, 1000], [49, 1000], [50, 2000], [109, 2000], [110, 1000], [149, 1000], [150, 0]]) {
    assert.equal(contributions.reduce((sum, contribution) => sum + feeAt(contribution, block), 0), expected);
  }
  assert.equal(contributions.reduce((sum, contribution) => {
    for (let block = 0; block < 200; block++) sum += feeAt(contribution, block);
    return sum;
  }, 0), 200 * FEE);
});

test('projection awards all issuance to a sole contribution and halves it with equal competition', () => {
  const draft = plan({ address: 'own', budget_sats: 300000, start_block: 11 });
  const competing = plan({ address: 'other', budget_sats: 300000, start_block: 11 });
  assert.equal(projectedReward([], draft, 'own'), 5000 * SPARKS);
  assert.equal(projectedReward([competing], draft, 'own'), 2500 * SPARKS);
});

test('projection uses each known competitor’s actual expiry rather than extrapolating its current rate', () => {
  const draft = plan({ address: 'own', budget_sats: 300000, start_block: 11 });
  const short = plan({ address: 'other', budget_sats: 150000, duration: 50, start_block: 11 });
  const long = plan({ address: 'other', budget_sats: 3000000, duration: 1000, start_block: 11 });
  assert.equal(burnAt(short, 11), burnAt(long, 11));
  assert.equal(projectedReward([short], draft, 'own'), 3750 * SPARKS);
  assert.equal(projectedReward([long], draft, 'own'), 2500 * SPARKS);
  assert.equal(burnAt(short, 60), 2000);
  assert.equal(burnAt(short, 61), 0, 'the last funded block is inclusive');
  assert.equal(burnAt(short, 999), 0);
});

test('future other-user schedules enter forecasts while expired contributions do not', () => {
  const draft = plan({ address: 'own', budget_sats: 300000, start_block: 11 });
  const expired = plan({ address: 'other', start_block: -90, budget_sats: 300000 });
  const scheduled = plan({ address: 'other', start_block: 61, duration: 50, budget_sats: 150000 });
  assert.equal(projectedReward([expired, scheduled], draft, 'own'), 3750 * SPARKS);
  assert.equal(burnAt(expired, 11), 0);
  assert.equal(burnAt(scheduled, 60), 0);
  assert.equal(burnAt(scheduled, 61), 2000);
  assert.equal(burnAt(scheduled, 110), 2000);
  assert.equal(burnAt(scheduled, 111), 0);
});

test('known own contributions reduce the draft’s allocated share and expire on schedule', () => {
  const draft = plan({ address: 'own', budget_sats: 300000, start_block: 11 });
  const existing = plan({ address: 'own', start_block: 11, budget_sats: 150000, duration: 50 });
  assert.equal(projectedReward([existing], draft, 'own'), 3750 * SPARKS);
  assert.equal(burnAt(existing, 60), 2000);
  assert.equal(burnAt(existing, 61), 0);
  const later = { ...existing, start_block: 61 };
  assert.equal(projectedReward([later], draft, 'own'), 3750 * SPARKS, 'known future overlap is included even if it has not begun');
});

test('projections preserve each actual remainder block and account for delayed windows beyond the chart', () => {
  const competing = plan({ address: 'other' });
  const draft = plan({ address: 'own', start_block: 15, duration: 1000, budget_sats: 3000000 });
  const estimate = projectedReward([competing], draft, 'own');
  assert.equal(burnAt(competing, 2000), 0, 'expired competition is not extrapolated');
  assert.equal(estimate, (95 * 25 + 905 * 50) * SPARKS, 'only the actual 95 overlapping blocks share rewards');
  assert.equal(burnAt(competing, 14), 2001, 'historical samples remain factual');
  const later = { ...draft, start_block: 5000 };
  assert.equal(projectedReward([competing], later, 'own'), 50000 * SPARKS, 'a delayed draft is evaluated over its full future duration');
});

test('event-based forecasts match a per-block reference across randomized starts, expiries, overlaps, and integer remainders', () => {
  let seed = 74219;
  const random = limit => { seed = (seed * 1664525 + 1013904223) >>> 0; return seed % limit; };
  const schedule = () => {
    const duration = random(100) + 1;
    return plan({ duration, start_block: random(150), budget_sats: duration * (FEE + 1) + random(100000) });
  };
  for (let sample = 0; sample < 300; sample++) {
    const draft = schedule();
    const contributions = Array.from({ length: random(8) }, schedule);
    let expected = 0;
    for (let block = draft.start_block; block < draft.start_block + draft.duration; block++) {
      const own = burnAt(draft, block);
      const total = contributions.reduce((sum, contribution) => sum + burnAt(contribution, block), own);
      expected += 50 * SPARKS * own / total;
    }
    const actual = projectedReward(contributions, draft);
    assert.ok(Math.abs(actual - expected) <= Math.max(1, expected) * 1e-12, `schedule ${sample}: ${actual} differs from ${expected}`);
  }
});

test('delegated forecasts match per-block eligibility across overlapping providers, restarts, self-delegation, and remainders', () => {
  let seed = 91041;
  const random = limit => { seed = (seed * 1664525 + 1013904223) >>> 0; return seed % limit; };
  const schedule = () => {
    const mode = ['independent', 'service', 'delegating'][random(3)];
    const duration = random(50) + 1;
    const fee = mode === 'delegating' ? 0 : FEE;
    return plan({ mode, user_id: random(3) + 1, service_user_id: random(3) + 1, commission_bps: random(10001), duration, start_block: random(100), budget_sats: duration * (fee + 1) + random(100000) });
  };
  const eligible = (contribution, block, all) => contribution.mode !== 'delegating' || all.some(provider => provider.mode === 'service' && provider.user_id === contribution.service_user_id && block >= provider.start_block && block < provider.start_block + provider.duration);
  for (let sample = 0; sample < 300; sample++) {
    const draft = schedule();
    const contributions = Array.from({ length: random(10) }, schedule);
    const all = [...contributions, draft];
    let expected = 0;
    for (let block = draft.start_block; block < draft.start_block + draft.duration; block++) {
      if (!eligible(draft, block, all)) continue;
      const total = all.reduce((sum, contribution) => sum + (eligible(contribution, block, all) ? burnAt(contribution, block) : 0), 0);
      expected += 50 * SPARKS * burnAt(draft, block) / total;
    }
    if (draft.mode === 'delegating' && draft.user_id !== draft.service_user_id) expected *= (10000 - draft.commission_bps) / 10000;
    const actual = projectedReward(contributions, draft);
    assert.ok(Math.abs(actual - expected) <= Math.max(1, expected) * 1e-12, `delegated schedule ${sample}: ${actual} differs from ${expected}`);
  }
});

test('trillion-block delegation forecasts follow provider boundaries without per-block work', () => {
  const duration = 1000000000000;
  const draft = plan({ mode: 'delegating', service_user_id: 2, commission_bps: 200, start_block: 100, duration, budget_sats: duration });
  const provider = plan({ mode: 'service', user_id: 2, start_block: 100 + duration / 2, duration: duration / 2, budget_sats: duration / 2 * (FEE + 1) });
  const started = performance.now();
  assert.equal(projectedReward([provider], draft), duration / 2 * 25 * SPARKS * .98);
  assert.ok(performance.now() - started < 1000);
});

test('self-delegation forecasts return commission to the same owner, including a one-hundred-percent commission', () => {
  const provider = plan({ mode: 'service', user_id: 1, budget_sats: 300000, start_block: 11 });
  for (const commission_bps of [0, 200, 10000]) {
    const draft = plan({ mode: 'delegating', user_id: 1, service_user_id: 1, commission_bps, budget_sats: 200000, start_block: 11 });
    assert.equal(projectedReward([provider], draft), 2500 * SPARKS);
    assert.equal(projectedReward([{ ...provider, start_block: 111 }], draft), 0, 'self-delegation still needs active Service endorsements');
  }
});

test('trillion-block forecasts scale with schedule events rather than the number of blocks', () => {
  const duration = 1000000000000;
  const draft = plan({ duration, start_block: 1000000000000, budget_sats: duration * (FEE + 1) });
  const start = performance.now();
  assert.equal(projectedReward([], draft), duration * 50 * SPARKS);
  assert.equal(projectedReward([{ ...draft }], draft), duration * 25 * SPARKS);
  assert.ok(performance.now() - start < 1000, 'forecasting a large manual duration must not freeze the page');
});

test('large burn-rate events cancel exactly before tiny competitors and lone draft blocks', () => {
  const draft = plan({ duration: 4, start_block: 11, budget_sats: 4 * (FEE + 1) });
  const large = plan({ duration: 1, start_block: 11, budget_sats: Number.MAX_SAFE_INTEGER });
  const tiny = plan({ duration: 1, start_block: 12, budget_sats: FEE + 1 });
  const expected = 50 * SPARKS * (1 / (Number.MAX_SAFE_INTEGER - FEE + 1) + 0.5 + 2);
  assert.equal(projectedReward([large, tiny], draft), expected);
});

test('a contribution finishes on the last inclusive block, not one block later', () => {
  assert.equal(contributionStatus(plan(), 9), 'scheduled');
  assert.equal(contributionStatus(plan(), 10), 'active');
  assert.equal(contributionStatus(plan(), 108), 'active');
  assert.equal(contributionStatus(plan(), 109), 'finished');
});

test('the twelve-color fixed palette preserves the originals and their rounded RGB midpoints with stable modulo mapping', () => {
  const original = ['#ffd800', '#ffc200', '#ff9c00', '#ff6922', '#ff4242', '#fff000'];
  const middle = original.map((color, index) => `#${[1, 3, 5].map(offset => Math.round((parseInt(color.slice(offset, offset + 2), 16) + parseInt(original[(index + 1) % original.length].slice(offset, offset + 2), 16)) / 2).toString(16).padStart(2, '0')).join('')}`);
  assert.equal(PALETTE.length, 12);
  assert.equal(new Set(PALETTE).size, 12);
  assert.deepEqual(new Set(PALETTE), new Set([...original, ...middle]));
  assert.deepEqual(PALETTE, ['#ffd800', '#ff6922', '#ffc200', '#ff4242', '#ff9c00', '#ffe400', '#ff9921', '#fff000', '#ffaf00', '#ff5632', '#ffcd00', '#ff8311']);
  assert.equal(userColor(1), PALETTE[1], 'the first user has ID one, not zero');
  assert.equal(userColor(12), PALETTE[0]);
  assert.equal(userColor(13), PALETTE[1]);
  for (const id of [0, 1, 2, 3, 4, 5, 6, 7, 11, 12, 13, 18, 26, 999, Number.MAX_SAFE_INTEGER]) assert.equal(userColor(id), PALETTE[id % PALETTE.length]);
  assert.doesNotMatch(source, /Math\.random|shuffledPalette|addressColors|paletteKey/);
});

test('delegating user colors use the specified stable blue palette while services remain warm', () => {
  for (const userId of [0, 1, 2, 5, 11, 12, 13, 26, Number.MAX_SAFE_INTEGER]) {
    assert.equal(userColor(userId, 'delegating'), BLUE_PALETTE[userId % BLUE_PALETTE.length]);
    assert.equal(userColor(userId, 'independent'), PALETTE[userId % PALETTE.length]);
    assert.equal(userColor(userId, 'service'), PALETTE[userId % PALETTE.length]);
  }
});

test('every referenced UI element exists once in the page', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
  assert.equal(ids.length, new Set(ids).size, 'duplicate element IDs');
  for (const [, id] of source.matchAll(/element\('([^']+)'\)/g)) assert.ok(ids.includes(id), `missing #${id}`);
  assert.doesNotMatch(html, /\bcomposition(?:-label)?\b|type="checkbox"/, 'contribution colors have no toggle');
});

test('both pages keep Dashboard and Mint links together on the left and mark the active page', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  const header = html.match(/<header\b[^>]*>[\s\S]*?<\/header>/)[0];
  assert.match(header, /<a\b[^>]*id="nav-dashboard"[^>]*href="\/"[^>]*>Dashboard<\/a>/);
  assert.match(header, /<a\b[^>]*id="nav-mint"[^>]*href="\/mint"[^>]*>Mint<\/a>/);
  assert.ok(header.indexOf('id="nav-dashboard"') < header.indexOf('id="nav-mint"'));
  assert.ok(header.indexOf('id="nav-mint"') < header.indexOf('class="topbar-actions"'));
  assert.doesNotMatch(header, /id="navigation"/);
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    assert.equal(page.elements.get(pathname === '/' ? 'nav-dashboard' : 'nav-mint')['aria-current'], 'page');
    assert.notEqual(page.elements.get(pathname === '/' ? 'nav-mint' : 'nav-dashboard')['aria-current'], 'page');
    assert.equal(page.elements.get('nav-dashboard').textContent, 'Dashboard');
    assert.equal(page.elements.get('nav-mint').textContent, 'Mint');
    assert.equal(page.elements.get('page-title').textContent, pathname === '/' ? 'Mint playground' : 'Mint');
  }
});

test('minter names load only on the mint page after the wallet has been saved, not on polls or display changes', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  assert.match(html, /<input\b[^>]*id="minter-name"[^>]*maxlength="64"/);
  assert.match(html, /<[^>]*id="minter-name-status"[^>]*role="status"/);
  const dashboard = await openPage('/');
  assert.equal(dashboard.elements.get('minter-profile').hidden, true);
  assert.equal(profileRequests(dashboard).length, 0);
  let finishWallet;
  const walletReady = new Promise(resolve => { finishWallet = resolve; });
  const storage = new Map();
  const page = await openPage('/mint', storage, undefined, walletReady, { profile: body => {
    assert.equal(JSON.parse(storage.get('flame-mint-playground-wallet-v1')).address, body.address, 'identity must be saved before registration');
    return profileResponse();
  } });
  assert.equal(page.elements.get('minter-profile').hidden, false);
  assert.equal(page.elements.get('minter-name').disabled, true);
  assert.equal(page.elements.get('save-minter-name').disabled, true);
  assert.equal(profileRequests(page).length, 0);
  finishWallet();
  await page.settle();
  assert.equal(page.elements.get('minter-name').value, 'Minter 1');
  assert.equal(page.elements.get('minter-name').disabled, false);
  assert.deepEqual(profileRequests(page).map(request => JSON.parse(request.options.body)), [{ address: 'tf1demo-address' }]);
  page.elements.get('minter-name').value = 'An unfinished edit';
  await page.refresh();
  await page.event('unit-btc', 'click');
  await page.event('time-days', 'click');
  await page.event('zoom-out', 'click');
  await page.timer(60000);
  assert.equal(page.elements.get('minter-name').value, 'An unfinished edit', 'rendering must not overwrite an in-progress edit');
  assert.equal(profileRequests(page).length, 1, 'profile lookup is not repeated by background polling');
});

test('minter names are saved explicitly, trimmed by the service, and loaded from the service after reload', async () => {
  let savedName = 'Minter 1';
  const profile = body => {
    if (body.name !== undefined) savedName = body.name.trim();
    return profileResponse(savedName);
  };
  const storage = new Map();
  const page = await openPage('/mint', storage, undefined, undefined, { profile });
  const name = '<img src=x onerror=alert(1)> & Alice';
  page.elements.get('minter-name').value = `  ${name}  `;
  await page.refresh();
  assert.equal(profileRequests(page).length, 1, 'typing and polling do not auto-save');
  await page.event('minter-profile', 'submit');
  assert.deepEqual(JSON.parse(profileRequests(page).at(-1).options.body), { address: 'tf1demo-address', name });
  assert.equal(page.elements.get('minter-name').value, name, 'the validated display name is plain input text');
  assert.equal(savedName, name);
  assert.equal(page.elements.get('wallet-address').textContent, 'tf1demo-address');
  assert.equal(page.storage.get('flame-mint-playground-wallet-v1').includes(name), false, 'display names do not modify wallet keys');
  assert.equal(page.requests.filter(request => request.url === '/api/contributions').length, 0);
  const reloaded = await openPage('/mint', storage, undefined, undefined, { profile });
  assert.equal(reloaded.elements.get('minter-name').value, name);
  assert.deepEqual(profileRequests(reloaded).map(request => JSON.parse(request.options.body)), [{ address: 'tf1demo-address' }]);
  assert.doesNotMatch(source, /\.innerHTML\s*=/, 'user names are never interpreted as markup');
});

test('a busy name save locks only the profile controls and errors keep the edit available for retry', async () => {
  let finishSave;
  let calls = 0;
  const page = await openPage('/mint', new Map(), undefined, undefined, { profile: () => ++calls === 1 ? profileResponse() : new Promise(resolve => { finishSave = resolve; }) });
  page.elements.get('minter-name').value = 'Alice';
  const saving = page.event('minter-profile', 'submit');
  await page.settle();
  assert.equal(page.elements.get('minter-name').disabled, true);
  assert.equal(page.elements.get('save-minter-name').disabled, true);
  assert.equal(page.elements.get('submit').disabled, false, 'name registration is not a prerequisite for minting');
  await page.event('unit-usd', 'click');
  assert.equal(page.elements.get('minter-name').value, 'Alice');
  finishSave({ ok: false, status: 400, json: async () => ({ error: 'Invalid minter name' }) });
  await saving;
  assert.equal(page.elements.get('minter-name').value, 'Alice');
  assert.equal(page.elements.get('minter-name').disabled, false);
  assert.equal(page.elements.get('save-minter-name').disabled, false);
  assert.match(page.elements.get('minter-name-status').textContent, /Invalid minter name/);
  const retrying = page.event('minter-profile', 'submit');
  await page.settle();
  finishSave(profileResponse('Alice'));
  await retrying;
  assert.equal(profileRequests(page).length, 3);
  assert.equal(page.elements.get('minter-name').value, 'Alice');
  assert.equal(page.elements.get('save-minter-name').disabled, false);
});

test('profile lookup failures can be retried without replacing or disabling the wallet', async () => {
  let calls = 0;
  const page = await openPage('/mint', new Map(), undefined, undefined, { profile: () => {
    if (++calls === 1) throw new Error('Name service offline');
    return profileResponse('Minter 1');
  } });
  assert.match(page.elements.get('minter-name-status').textContent, /Name service offline/);
  assert.equal(page.elements.get('wallet-status').textContent, 'Saved locally. Keys stay in this browser.');
  assert.equal(page.elements.get('submit').disabled, false);
  assert.equal(page.elements.get('save-minter-name').disabled, false);
  await page.event('minter-profile', 'submit');
  assert.equal(page.elements.get('minter-name').value, 'Minter 1');
  assert.deepEqual(profileRequests(page).map(request => JSON.parse(request.options.body)), [{ address: 'tf1demo-address' }, { address: 'tf1demo-address' }]);
  const broken = await openPage('/mint', new Map([['flame-mint-playground-wallet-v1', '{bad json']]));
  assert.equal(profileRequests(broken).length, 0);
  assert.equal(broken.elements.get('minter-name').disabled, true);
  assert.equal(broken.elements.get('save-minter-name').disabled, true);
});

test('a stale profile response cannot rename a replacement wallet after reset', async () => {
  let finishOld;
  let identities = 0;
  const page = await openPage('/mint', new Map(), undefined, undefined, {
    walletAddress: () => ++identities === 1 ? 'old-wallet' : 'new-wallet',
    profile: body => body.address === 'old-wallet' ? new Promise(resolve => { finishOld = resolve; }) : profileResponse('Minter 2', 2),
  });
  assert.equal(page.elements.get('wallet-address').textContent, 'old-wallet');
  await page.event('reset-wallet', 'click');
  assert.equal(page.elements.get('wallet-address').textContent, 'new-wallet');
  assert.equal(page.elements.get('minter-name').value, 'Minter 2');
  finishOld(profileResponse('Old name', 1));
  await page.settle();
  assert.equal(page.elements.get('minter-name').value, 'Minter 2');
  assert.equal(page.elements.get('wallet-address').textContent, 'new-wallet');
  assert.equal(page.elements.get('minter-name').disabled, false);
  assert.equal(page.elements.get('save-minter-name').disabled, false);
});

test('mint modes expose only their own options and delegated estimates use net rewards with no mining fees', async () => {
  const page = await openPage('/mint');
  assert.equal(page.elements.get('mode-independent').checked, true);
  assert.equal(page.elements.get('service-options').hidden, true);
  assert.equal(page.elements.get('delegation-options').hidden, true);
  await page.event('mode-service', 'change');
  assert.equal(page.elements.get('mode-service').checked, true);
  assert.equal(page.elements.get('service-options').hidden, false);
  assert.equal(page.elements.get('delegation-options').hidden, true);
  assert.equal(page.elements.get('commission').value, '2');
  for (const value of ['-1', '100.01', '0.001', 'bad']) {
    page.elements.get('commission').value = value;
    await page.event('commission', 'input');
    assert.equal(page.elements.get('submit').disabled, true, `invalid commission ${value}`);
  }
  page.elements.get('commission').value = '2';
  await page.event('commission', 'input');
  page.snapshot.services = [{ user_id: 2, name: '<Service 2>', commission_bps: 200, active: true }];
  page.snapshot.contributions = [plan({ id: 2, user_id: 2, address: 'provider', mode: 'service', commission_bps: 200, duration: 500, start_block: 11, budget_sats: 1500000 })];
  await page.refresh();
  await page.event('mode-delegating', 'change');
  assert.equal(page.elements.get('service-options').hidden, true);
  assert.equal(page.elements.get('delegation-options').hidden, false);
  assert.equal(page.elements.get('submit').disabled, true, 'a provider must be selected explicitly');
  assert.ok(page.elements.get('service-select').children.some(option => String(option.value) === '2' && option.textContent.includes('<Service 2>') && option.textContent.includes('2%')));
  page.elements.get('service-select').value = '2';
  await page.event('service-select', 'change');
  assert.equal(page.elements.get('submit').disabled, false);
  assert.equal(page.elements.get('projected-fees').textContent, '0 sat');
  assert.equal(page.elements.get('projected-burn').textContent, '1,000,000 sat');
  assert.equal(page.elements.get('projected-reward').textContent, '12,250');
  assert.equal(page.elements.get('projected-rate').textContent, '81 sat / FLM');
  page.snapshot.services[0].active = false;
  page.snapshot.contributions = [];
  await page.refresh();
  assert.equal(page.elements.get('submit').disabled, false, 'inactive service offers remain available for scheduling');
  assert.equal(page.elements.get('projected-reward').textContent, '0');
  assert.equal(page.elements.get('projected-burn').textContent, '1,000,000 sat', 'inactive delegation is still spent');
  assert.equal(page.elements.get('projected-rate').textContent, '—');
});

test('commission is saved on the service identity and never embedded in a contribution request', async () => {
  const page = await openPage('/mint');
  await page.event('mode-service', 'change');
  page.elements.get('commission').value = '12.34';
  await page.event('commission', 'input');
  assert.equal(page.elements.get('submit').disabled, true, 'unsaved identity changes must not silently travel inside a contribution');
  await page.event('save-commission', 'click');
  assert.deepEqual(JSON.parse(profileRequests(page).at(-1).options.body), { address: 'tf1demo-address', service_commission_bps: 1234 });
  await page.event('contribution-form', 'submit');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(request.mode, 'service');
  assert.equal(Object.hasOwn(request, 'commission_bps'), false);
  assert.equal(Object.hasOwn(request, 'service_user_id'), false);
  const independent = await openPage('/mint');
  await independent.event('contribution-form', 'submit');
  const plain = JSON.parse(independent.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(plain.mode ?? 'independent', 'independent');
  assert.equal(Object.hasOwn(plain, 'service_user_id'), false);
});

test('the mint form estimates self-delegation at gross reward when all commission returns to its owner', async () => {
  const page = await openPage('/mint');
  page.snapshot.services = [{ user_id: 1, name: 'My service', commission_bps: 10000, active: true }];
  page.snapshot.contributions = [plan({ id: 1, user_id: 1, address: 'tf1demo-address', mode: 'service', duration: 500, start_block: 11, budget_sats: 1500000 })];
  await page.refresh();
  await page.event('mode-delegating', 'change');
  page.elements.get('service-select').value = '1';
  await page.event('service-select', 'change');
  assert.equal(page.elements.get('projected-reward').textContent, '12,500');
  assert.equal(page.elements.get('projected-rate').textContent, '80 sat / FLM');
  assert.equal(page.elements.get('projected-fees').textContent, '0 sat');
  assert.match(page.elements.get('projection-note').textContent, /commission returns to you/i);
});

test('saved delegation retries preserve the provider while commission follows its current identity rate', async () => {
  const storage = new Map();
  const page = await openPage('/mint', storage, async () => { throw new Error('Response lost'); });
  page.snapshot.services = [{ user_id: 2, name: 'Service 2', commission_bps: 200, active: false }];
  await page.refresh();
  await page.event('mode-delegating', 'change');
  page.elements.get('service-select').value = '2';
  await page.event('service-select', 'change');
  await page.event('contribution-form', 'submit');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(request.mode, 'delegating');
  assert.equal(request.service_user_id, 2);
  assert.equal(Object.hasOwn(request, 'commission_bps'), false);
  for (const id of ['mode-independent', 'mode-service', 'mode-delegating', 'commission', 'service-select']) assert.equal(page.elements.get(id).disabled, true);
  const retry = await openPage('/mint', storage);
  assert.equal(retry.elements.get('mode-delegating').checked, true);
  assert.equal(retry.elements.get('submit').disabled, false, 'an accepted-but-unconfirmed request can be retried after its provider disappears');
  retry.snapshot.services = [{ user_id: 2, name: 'Changed service', commission_bps: 9900, active: false }];
  await retry.refresh();
  const selected = retry.elements.get('service-select').children.find(option => String(option.value) === '2');
  assert.match(selected.textContent, /99%/, 'even a pending retry shows the provider’s current identity rate');
  retry.elements.get('commission').value = '99';
  retry.elements.get('service-select').value = '999';
  await retry.event('mode-service', 'change');
  await retry.event('unit-btc', 'click');
  await retry.event('time-days', 'click');
  await retry.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(retry.requests.find(request => request.url === '/api/contributions').options.body), request);
  const unknown = await openPage('/mint');
  await unknown.event('mode-delegating', 'change');
  unknown.elements.get('service-select').value = '999';
  await unknown.event('service-select', 'change');
  assert.equal(unknown.elements.get('submit').disabled, true, 'unknown offers cannot create new delegations');
});

test('service option nodes survive unchanged renders while rate changes preserve selection and update pending previews', async () => {
  const page = await openPage('/mint', new Map(), async () => { throw new Error('Response lost'); });
  page.snapshot.services = [
    { user_id: 2, name: 'Service A', commission_bps: 200, active: false },
    { user_id: 3, name: 'Service B', commission_bps: 300, active: false },
  ];
  await page.refresh();
  await page.event('mode-delegating', 'change');
  const select = page.elements.get('service-select');
  select.value = '3';
  await page.event('service-select', 'change');
  let options = [...select.children];
  await page.refresh();
  await page.event('unit-btc', 'click');
  await page.event('time-days', 'click');
  assert.ok(select.children.every((option, index) => option === options[index]), 'unchanged renders do not interrupt the native picker by replacing its option nodes');
  assert.equal(select.value, '3');
  page.snapshot.services[0].name = 'Renamed A';
  await page.refresh();
  assert.notEqual(select.children[0], options[0], 'a changed service name rebuilds the catalog');
  assert.ok(select.children.some(option => option.textContent.includes('Renamed A')));
  assert.equal(select.value, '3');
  options = [...select.children];
  page.snapshot.services[1].commission_bps = 450;
  await page.refresh();
  assert.notEqual(select.children[0], options[0], 'a changed commission rebuilds the catalog');
  assert.equal(select.value, '3');
  assert.match(select.children.find(option => option.value === '3').textContent, /4\.5%/);
  await page.event('contribution-form', 'submit');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(request.service_user_id, 3);
  assert.equal(Object.hasOwn(request, 'commission_bps'), false);
  page.snapshot.services[1].commission_bps = 900;
  await page.refresh();
  const pendingOption = select.children.find(option => option.value === '3');
  assert.match(pendingOption.textContent, /9%/, 'pending previews follow the current identity instead of freezing a quote');
  await page.refresh();
  await page.event('unit-sat', 'click');
  assert.equal(select.children.find(option => option.value === '3'), pendingOption);
});

test('old saved commission quotes are removed from retry bodies without changing request identity or payment terms', async () => {
  for (const mode of ['service', 'delegating']) {
    const legacy = { address: 'tf1demo-address', request_id: `legacy-${mode}-request`, budget_sats: 1000000, duration: 500, delay_blocks: 17, mode, commission_bps: 1234, ...(mode === 'delegating' ? { service_user_id: 2 } : {}) };
    const storage = new Map([['flame-mint-playground-pending-v1', JSON.stringify(legacy)]]);
    const page = await openPage('/mint', storage);
    assert.equal(page.elements.get('submit').disabled, false);
    await page.event('contribution-form', 'submit');
    const { commission_bps, ...expected } = legacy;
    assert.equal(commission_bps, 1234);
    assert.deepEqual(JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body), expected);
  }
});

test('a pending delegation forecast immediately reprices at the provider’s current identity rate', async () => {
  const page = await openPage('/mint', new Map(), async () => { throw new Error('Response lost'); });
  page.snapshot.services = [{ user_id: 2, name: 'Service 2', commission_bps: 200, active: true }];
  page.snapshot.contributions = [plan({ id: 2, user_id: 2, address: 'provider', mode: 'service', duration: 500, start_block: 11, budget_sats: 1500000 })];
  await page.refresh();
  await page.event('mode-delegating', 'change');
  page.elements.get('service-select').value = '2';
  await page.event('service-select', 'change');
  assert.equal(page.elements.get('projected-reward').textContent, '12,250');
  await page.event('contribution-form', 'submit');
  const original = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(Object.hasOwn(original, 'commission_bps'), false);
  page.snapshot.services[0].commission_bps = 5000;
  await page.refresh();
  assert.equal(page.elements.get('projected-reward').textContent, '6,250');
  assert.equal(page.elements.get('projected-rate').textContent, '160 sat / FLM');
  await page.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(page.requests.filter(request => request.url === '/api/contributions').at(-1).options.body), original);
});

test('commission saves preserve unsaved edits, isolate failures, and refresh state immediately', async () => {
  let finishSave;
  const page = await openPage('/mint', new Map(), undefined, undefined, { profile: body => Object.hasOwn(body, 'service_commission_bps') ? new Promise(resolve => { finishSave = resolve; }) : profileResponse('Minter 1', 1, 200) });
  await page.event('mode-service', 'change');
  page.elements.get('commission').value = '50';
  await page.event('commission', 'input');
  page.elements.get('minter-name').value = 'An unsaved name';
  await page.refresh();
  await page.event('unit-btc', 'click');
  assert.equal(page.elements.get('commission').value, '50', 'polls do not overwrite an edited global rate');
  assert.equal(page.elements.get('submit').disabled, true);
  assert.match(page.elements.get('budget-error').textContent, /save commission before submitting/i);
  const saving = page.event('save-commission', 'click');
  await page.settle();
  assert.equal(page.elements.get('commission').disabled, true);
  assert.equal(page.elements.get('save-commission').disabled, true);
  await page.event('mode-independent', 'change');
  assert.equal(page.elements.get('submit').disabled, false, 'a pending profile change does not stop independent minting');
  finishSave({ ok: false, status: 503, json: async () => ({ error: 'Cannot save rate' }) });
  await saving;
  assert.match(page.elements.get('commission-status').textContent, /Cannot save rate/);
  assert.equal(page.elements.get('commission').value, '50');
  assert.equal(page.elements.get('minter-name').value, 'An unsaved name');
  await page.event('mode-service', 'change');
  const before = stateRequests(page).length;
  const retrying = page.event('save-commission', 'click');
  await page.settle();
  page.snapshot.services = [{ user_id: 1, name: 'Minter 1', commission_bps: 5000, active: false }];
  finishSave(profileResponse('Minter 1', 1, 5000));
  await retrying;
  assert.deepEqual(JSON.parse(profileRequests(page).at(-1).options.body), { address: 'tf1demo-address', service_commission_bps: 5000 });
  assert.equal(stateRequests(page).length, before + 1, 'successful saves fetch the new rate without waiting for the periodic poll');
  assert.equal(page.elements.get('minter-name').value, 'An unsaved name', 'saving a rate must not save or discard a separate name edit');
  assert.equal(page.elements.get('commission').value, '50');
  assert.equal(page.elements.get('submit').disabled, false);
});

test('a pending Service contribution does not freeze the separate identity commission setting', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  assert.match(html, /<input\b[^>]*id="commission"[^>]*form="commission-profile"/, 'identity-rate validity is separate from payment-form validity');
  const page = await openPage('/mint', new Map(), async () => { throw new Error('Response lost'); });
  await page.event('mode-service', 'change');
  await page.event('contribution-form', 'submit');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(Object.hasOwn(request, 'commission_bps'), false);
  assert.equal(page.elements.get('mode-independent').disabled, true);
  assert.equal(page.elements.get('budget').disabled, true);
  assert.equal(page.elements.get('commission').disabled, false);
  page.elements.get('commission').value = 'bad';
  await page.event('commission', 'input');
  assert.equal(page.elements.get('submit').disabled, false, 'an invalid unsaved global rate cannot block retrying an existing payment');
  await page.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(page.requests.filter(request => request.url === '/api/contributions').at(-1).options.body), request);
  page.elements.get('commission').value = '25';
  await page.event('commission', 'input');
  await page.event('save-commission', 'click');
  assert.deepEqual(JSON.parse(profileRequests(page).at(-1).options.body), { address: 'tf1demo-address', service_commission_bps: 2500 });
  await page.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(page.requests.filter(request => request.url === '/api/contributions').at(-1).options.body), request);
});

test('an older in-flight snapshot cannot overwrite balances refreshed by a successful commission save', async () => {
  let page, deferNext = false, finishOld;
  page = await openPage('/mint', new Map(), undefined, undefined, {
    state: snapshot => {
      const frozen = JSON.parse(JSON.stringify(snapshot));
      const response = { ok: true, json: async () => frozen };
      if (!deferNext) return response;
      deferNext = false;
      return new Promise(resolve => { finishOld = () => resolve(response); });
    },
    profile: body => {
      if (Object.hasOwn(body, 'service_commission_bps')) {
        page.snapshot.services[0].commission_bps = body.service_commission_bps;
        page.snapshot.minters[0].reward_sparks = String(4321 * SPARKS);
        return profileResponse('Minter 1', 1, body.service_commission_bps);
      }
      return profileResponse('Minter 1', 1, 200);
    },
  });
  page.snapshot.services = [{ user_id: 1, name: 'Minter 1', commission_bps: 200, active: false }];
  page.snapshot.minters = [{ user_id: 1, address: 'tf1demo-address', name: 'Minter 1', mode: 'service', commission_bps: 200, contribution_count: 1, burned_sats: '100000', fees_sats: '1000', reward_sparks: String(1234 * SPARKS) }];
  await page.refresh();
  assert.equal(page.elements.get('total-reward').textContent, '1,234\u00a0FLM');
  deferNext = true;
  const oldPoll = page.refresh();
  await page.settle();
  await page.event('mode-service', 'change');
  page.elements.get('commission').value = '50';
  await page.event('commission', 'input');
  await page.event('save-commission', 'click');
  assert.equal(page.elements.get('total-reward').textContent, '4,321\u00a0FLM');
  finishOld();
  await oldPoll;
  assert.equal(page.elements.get('total-reward').textContent, '4,321\u00a0FLM');
  assert.equal(page.elements.get('commission').value, '50');
});

test('reward address follows the contribution table outside the mint form, with the palette footer last', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  const main = html.match(/<main\b[^>]*>([\s\S]*?)<\/main>/)[1];
  const mint = main.match(/<section\b[^>]*id="mint-form"[^>]*>([\s\S]*?)<\/section>/)[1];
  assert.doesNotMatch(mint, /id="(?:wallet-panel|wallet-address|seed-details)"/, 'wallet controls do not belong to the contribution form');
  const wallet = main.match(/<section\b[^>]*id="wallet-panel"[^>]*>[\s\S]*?<\/section>/)[0];
  assert.match(wallet, /class="panel wallet"/);
  assert.match(wallet, /\bhidden(?:\s|>)/);
  assert.match(wallet, /aria-labelledby="wallet-title"/);
  assert.match(wallet, /<h[23]\b[^>]*id="wallet-title"[^>]*>Reward address \(testnet\)</);
  for (const id of ['wallet-status', 'wallet-address', 'seed-details', 'seed-phrase', 'retry-wallet', 'reset-wallet']) assert.match(wallet, new RegExp(`id="${id}"`));
  assert.match(wallet, /<details\b[^>]*id="seed-details"(?![^>]*\bopen)[^>]*>/, 'the recovery phrase is collapsed by default');
  assert.ok(main.indexOf('id="contribution-totals"') < main.indexOf('id="wallet-panel"'));
  assert.ok(main.indexOf('id="wallet-panel"') < main.indexOf('id="palette-preview"'));
  const footer = main.match(/<div\b[^>]*id="palette-preview"[^>]*>[\s\S]*?<\/div>\s*$/)[0];
  assert.match(footer, /\brole="list"/);
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    assert.equal(page.elements.get('wallet-panel').hidden, pathname !== '/mint');
    assert.equal(page.elements.get('mint-form').hidden, pathname !== '/mint');
    assert.equal(page.elements.get('palette-preview').hidden, false);
    assert.equal(page.elements.get('seed-phrase').textContent, '');
    assert.equal(page.storage.has('flame-mint-playground-wallet-v1'), pathname === '/mint', 'the dashboard does not initialize a wallet merely to render the footer');
  }
});

test('both pages render exactly one accessible swatch per warm and blue color without accumulating swatches', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const preview = page.elements.get('palette-preview');
    const colors = [...PALETTE, ...BLUE_PALETTE];
    const check = () => {
      assert.equal(preview.children.length, colors.length);
      assert.deepEqual(preview.children.map(swatch => swatch.tag), colors.map(() => 'span'));
      assert.deepEqual(preview.children.map(swatch => swatch.style.background), colors);
      assert.deepEqual(preview.children.map(swatch => swatch.title), colors);
      assert.deepEqual(preview.children.map(swatch => swatch['aria-label']), colors);
      assert.ok(preview.children.every(swatch => swatch.role === 'listitem'));
    };
    check();
    await page.refresh();
    await page.refresh();
    await page.event('unit-btc', 'click');
    await page.event('time-days', 'click');
    await page.event('zoom-out', 'click');
    check();
  }
});

test('linear slider budgets conserve the complete budget at duration boundaries and representative single-block choices', () => {
  for (const budget_sats of [BUDGET_MIN, 200000, 500000, 1000000, 50000000, BUDGET_MAX]) {
    for (const duration of [DURATION_MIN, DURATION_MIN + 1, 500, 1440, DURATION_MAX - 1, DURATION_MAX]) {
      if (budget_sats < duration * (FEE + 1)) continue;
      const contribution = plan({ budget_sats, duration });
      let total = duration * FEE;
      for (let block = contribution.start_block; block < contribution.start_block + duration; block++) {
        const burn = burnAt(contribution, block);
        assert.ok(Number.isInteger(burn) && burn >= 1);
        total += burn;
      }
      assert.equal(total, budget_sats);
    }
  }
});

const quoteResponse = amount => ({ ok: true, json: async () => ({ data: { base: 'BTC', currency: 'USD', amount: String(amount) } }) });
const profileResponse = (name = 'Minter 1', user_id = 1, service_commission_bps = null) => ({ ok: true, json: async () => ({ user_id, name, service_commission_bps }) });
const profileRequests = page => page.requests.filter(request => request.url === '/api/minter');

async function openPage(pathname, storage = new Map(), post = async () => ({ id: 2, user_id: 1, start_block: 11 }), walletReady = Promise.resolve(), pageOptions = {}) {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  const draws = [];
  class Element {
    constructor(tag = 'div') { this.tag = tag; this.children = []; this.style = {}; this.handlers = {}; this.value = ''; this.hidden = false; this.disabled = false; this.checked = false; }
    append(...children) { for (const child of children) this.children.push(...(child.tag === 'fragment' ? child.children : [child])); }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    addEventListener(event, handler) { this.handlers[event] = handler; }
    setAttribute(name, value) { this[name] = value; }
    getContext() {
      return new Proxy({}, { get: (context, method) => (...args) => {
        draws.push({ method, args, fillStyle: context.fillStyle, strokeStyle: context.strokeStyle, globalAlpha: context.globalAlpha, canvas: this });
        if (method === 'createPattern') return { kind: 'pattern', canvas: args[0], repetition: args[1] };
        if (method === 'measureText') return { width: String(args[0]).length * 6 };
      } });
    }
    get clientWidth() { return 800; }
    get clientHeight() { return 275; }
    get options() { return this.tag === 'select' ? this.children : undefined; }
  }
  const elements = new Map([...html.matchAll(/<([a-z][a-z0-9]*)\b[^>]*\bid="([^"]+)"[^>]*>/g)].map(([, tag, id]) => [id, new Element(tag)]));
  for (const [tag, , id] of html.matchAll(/<([a-z][a-z0-9]*)\b[^>]*\bid="([^"]+)"[^>]*>/g)) {
    elements.get(id).hidden = /\bhidden(?:\s|>)/.test(tag);
    elements.get(id).disabled = /\bdisabled(?:\s|>)/.test(tag);
    elements.get(id).checked = /\bchecked(?:\s|>)/.test(tag);
    const value = tag.match(/\bvalue="([^"]*)"/);
    if (value) elements.get(id).value = value[1];
  }
  for (const [, id, text] of html.matchAll(/\bid="([^"]+)"[^>]*>([^<]*)</g)) elements.get(id).textContent = text.replaceAll('&nbsp;', '\u00a0');
  elements.get('budget').value = '1000000'; elements.get('duration').value = '500';
  elements.get('delay').value = '0';
  const requests = [], frames = [], timers = new Map(), abortTimeouts = [];
  let nextTimer = 0;
  let profileCommission = null;
  const snapshot = { height: 10, totals: { contribution_count: 1, burned_sats: '100000', fees_sats: '1000', reward_sparks: '50000000000' }, contributions: [{ id: 1, user_id: 1, address: 'tf1demo-address', budget_sats: 300000, duration: 100, start_block: 10, burned_sats: '2000', fees_sats: '1000', reward_sparks: '5000000000' }] };
  const context = vm.createContext({
    document: { getElementById: id => elements.get(id), createElement: tag => new Element(tag), createDocumentFragment: () => new Element('fragment') },
    location: { pathname }, window: { devicePixelRatio: 1 }, console,
    AbortSignal: { timeout: delay => { abortTimeouts.push(delay); return AbortSignal.timeout(delay); } },
    localStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value), removeItem: key => storage.delete(key) },
    crypto: { randomUUID: () => '11111111-1111-4111-8111-111111111111' },
    confirm: (...args) => pageOptions.confirm ? pageOptions.confirm(...args) : true,
    requestAnimationFrame: callback => frames.push(callback),
    ResizeObserver: class { observe() {} },
    setTimeout: (callback, delay) => { const id = ++nextTimer; timers.set(id, { callback, delay }); return id; },
    clearTimeout: id => timers.delete(id),
    walletModule: { default: async () => walletReady, generateMnemonic: () => 'a browser only recovery phrase', Wallet: { fromMnemonic: mnemonic => ({ nextAddress: () => ({ receivingAddress: pageOptions.walletAddress ? pageOptions.walletAddress(mnemonic) : 'tf1demo-address' }), free() {} }) } },
    fetch: async (url, options = {}) => {
      requests.push({ url, options });
      if (url === '/api/state' || url === '/api/state?full_history=true') return pageOptions.state ? pageOptions.state(snapshot, url) : { ok: true, json: async () => snapshot };
      if (url === 'https://api.coinbase.com/v2/prices/BTC-USD/spot') return pageOptions.quote ? pageOptions.quote() : quoteResponse(50000);
      if (url === '/api/minter') {
        const body = JSON.parse(options.body);
        if (Object.hasOwn(body, 'service_commission_bps')) profileCommission = body.service_commission_bps;
        return pageOptions.profile ? pageOptions.profile(body, options) : profileResponse(body.name?.trim() ?? 'Minter 1', 1, profileCommission);
      }
      assert.equal(url, '/api/contributions', 'unexpected request');
      return { ok: true, json: async () => post(JSON.parse(options.body)) };
    },
  });
  const script = source.replace(/^export /gm, '').replace("await import('/wallet/flamewallet_wasm.js')", 'await Promise.resolve(walletModule)');
  vm.runInContext(script, context);
  const settle = async () => { for (let i = 0; i < 4; i++) await new Promise(resolve => setImmediate(resolve)); while (frames.length) frames.shift()(); };
  await settle();
  return {
    elements, requests, storage, snapshot, draws, settle, timers, abortTimeouts,
    event: async (id, event) => { await elements.get(id).handlers[event]({ target: elements.get(id), preventDefault() {} }); await settle(); },
    timer: async delay => {
      const pending = [...timers].find(([, timer]) => timer.delay === delay);
      assert.ok(pending, `no ${delay}ms timer scheduled`);
      const [id, timer] = pending;
      timers.delete(id);
      await timer.callback();
      await settle();
    },
    refresh: async () => {
      const [id, timer] = [...timers].find(([, timer]) => timer.delay === 1000);
      timers.delete(id);
      await timer.callback();
      await settle();
    },
  };
}

test('both charts center now between five hundred past and future blocks by default', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  assert.match(html, /<canvas\b[^>]*aria-label="[^"]*Previous and next 500 blocks\./);
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const minting = pathname === '/mint';
    page.snapshot.height = minting ? 500 : 1500;
    page.snapshot.contributions = [plan({ id: 1, address: 'tf1demo-address', start_block: page.snapshot.height - 200, duration: 1000, budget_sats: 3000000 })];
    page.draws.length = 0;
    await page.refresh();
    assert.equal(page.elements.get('connection').textContent, 'Live');
    const draws = page.draws.filter(call => call.canvas === page.elements.get('chart'));
    const ticks = draws.filter(call => call.method === 'fillText' && call.args[2] === 267);
    assert.deepEqual(ticks.map(call => call.args[0]), minting ? ['0', '250', '500', '750', '1,000'] : ['1,000', '1,250', '1,500', '1,750', '2,000']);
    assert.deepEqual(ticks.map(call => call.args[1]), [48, 224, 400, 576, 752]);
    const shading = draws.find(call => call.method === 'fillRect' && call.fillStyle === '#ffffff30');
    assert.equal(shading.args[0], 400, 'future shading begins at the centered current block');
    assert.equal(shading.args[2], 352, 'future shading ends at the right edge');
    const pathBefore = index => {
      const start = draws.slice(0, index).findLastIndex(call => call.method === 'beginPath');
      return draws.slice(start + 1, index).filter(call => call.method === 'moveTo' || call.method === 'lineTo');
    };
    const nowLine = pathBefore(draws.findLastIndex(call => call.method === 'stroke' && call.strokeStyle === '#666'));
    assert.deepEqual(nowLine.map(call => call.args), [[400, 14], [400, 248]], 'now marks the center of both charts');
    const band = pathBefore(draws.findIndex(call => call.method === 'fill' && call.fillStyle === userColor(1)));
    assert.equal(band.length, 2003, 'a closed band contains 1,001 top and 1,001 bottom samples plus its starting point');
    assert.equal(Math.min(...band.map(call => call.args[0])), 48);
    assert.equal(Math.max(...band.map(call => call.args[0])), 752);
    if (minting) {
      assert.match(page.elements.get('chart')['aria-label'], /Previous and next 500 blocks[, .]/);
      assert.match(page.elements.get('chart-note').textContent, /known|scheduled|future/i);
      const draft = pathBefore(draws.findIndex(call => call.method === 'stroke' && call.strokeStyle === userColor(1)));
      assert.equal(draft.length, 501, 'draft sampling stays within the visible future window');
      assert.equal(draft[0].args[0], 400);
      assert.equal(draft.at(-1).args[0], 752);
    }
  }
});

test('zoom controls halve or double whole-block radii without touching accounting', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    page.snapshot.height = 6000;
    page.snapshot.contributions = [plan({ ...page.snapshot.contributions[0], start_block: 1, duration: 20000, budget_sats: 60000000 })];
    await page.refresh();
    const before = JSON.stringify(page.snapshot);
    const stateCount = stateRequests(page).length;
    const totals = ['total-burned', 'total-reward', 'total-rate'].map(id => page.elements.get(id).textContent);
    const estimate = ['projected-reward', 'projected-rate'].map(id => page.elements.get(id).textContent);
    const assertRadius = radius => {
      const draws = page.draws.filter(call => call.canvas === page.elements.get('chart'));
      const ticks = draws.filter(call => call.method === 'fillText' && call.args[2] === 267);
      assert.deepEqual(ticks.map(call => call.args[0]), Array.from({ length: 5 }, (_, index) => {
        const block = Math.round(6000 - radius + radius / 2 * index);
        return block < 0 ? '—' : new Intl.NumberFormat('en').format(block);
      }));
      assert.deepEqual(ticks.map(call => call.args[1]), [48, 224, 400, 576, 752]);
      assert.match(page.elements.get('chart-window').textContent, new RegExp(`${new Intl.NumberFormat('en').format(radius)} blocks`));
      assert.match(page.elements.get('chart')['aria-label'], new RegExp(`Previous and next ${radius} blocks`));
      let path = [];
      let band;
      let now;
      for (const call of draws) {
        if (call.method === 'beginPath') path = [];
        if (call.method === 'moveTo' || call.method === 'lineTo') path.push(call.args);
        if (call.method === 'fill' && call.fillStyle === userColor(1)) band = path;
        if (call.method === 'stroke' && call.strokeStyle === '#666') now = path;
      }
      if (radius <= 1000) assert.equal(band.length, 2 * (2 * radius + 1) + 1, 'nearby blocks retain exact per-block samples');
      else assert.ok(band.length < 50, 'wide charts sample schedule events rather than every block');
      assert.equal(band[1][0], 48);
      assert.equal(Math.max(...band.map(point => point[0])), 752);
      assert.deepEqual(now, [[400, 14], [400, 248]], 'now stays centered at every zoom');
      assert.equal(page.elements.get('zoom-in').disabled, radius === ZOOM_MIN);
      assert.equal(page.elements.get('zoom-out').disabled, radius === Number.MAX_SAFE_INTEGER);
      assert.equal(JSON.stringify(page.snapshot), before);
      assert.equal(stateRequests(page).length, stateCount, 'zoom never fetches another snapshot');
      assert.deepEqual(['total-burned', 'total-reward', 'total-rate'].map(id => page.elements.get(id).textContent), totals);
      assert.deepEqual(['projected-reward', 'projected-rate'].map(id => page.elements.get(id).textContent), estimate);
      assert.equal(page.elements.get('budget').value, '1000000');
      assert.equal(page.elements.get('duration').value, '500');
      assert.equal(page.elements.get('delay').value, '0');
    };
    assert.equal(page.storage.has(ZOOM_KEY), false, 'default zoom does not need a storage write');
    for (const radius of [250, 125, 63, 32, 16, 10]) {
      page.draws.length = 0;
      await page.event('zoom-in', 'click');
      assertRadius(radius);
      assert.equal(page.storage.get(ZOOM_KEY), String(radius));
    }
    await page.event('zoom-in', 'click');
    assert.equal(page.storage.get(ZOOM_KEY), '10', 'an off-limit click cannot shrink below ten blocks');
    for (const radius of [20, 40, 80, 160, 320, 640, 1280, 2560, 5120, 10240, 20480]) {
      page.draws.length = 0;
      await page.event('zoom-out', 'click');
      assertRadius(radius);
      assert.equal(page.storage.get(ZOOM_KEY), String(radius));
    }
  }
});

test('zoom persists across both pages independently from currency and calendar display', async () => {
  const page = await openPage('/mint');
  await page.event('unit-btc', 'click');
  await page.event('time-days', 'click');
  await page.event('zoom-out', 'click');
  assert.equal(page.storage.get(ZOOM_KEY), '1000');
  assert.equal(page.storage.get(TIME_KEY), 'days');
  assert.equal(page.storage.get(UNIT_KEY), 'BTC');
  assert.match(page.elements.get('chart-window').textContent, /6\.944 days/);
  assert.match(page.elements.get('chart')['aria-label'], /Previous and next 1000 blocks/);
  for (const pathname of ['/', '/mint']) {
    const other = await openPage(pathname, page.storage);
    other.snapshot.height = 8785;
    await other.refresh();
    assert.equal(other.elements.get('unit-btc')['aria-pressed'], 'true');
    assert.equal(other.elements.get('time-days')['aria-pressed'], 'true');
    assert.match(other.elements.get('chart-window').textContent, /6\.944 days/);
    const ticks = other.draws.filter(call => call.canvas === other.elements.get('chart') && call.method === 'fillText' && call.args[2] === 267).slice(-5);
    assert.deepEqual(ticks.map(call => call.args[0]), [7785, 8285, 8785, 9285, 9785].map(blockDate));
    other.draws.length = 0;
    await other.event('time-blocks', 'click');
    assert.match(other.elements.get('chart-window').textContent, /1,000 blocks/);
    assert.equal(other.storage.get(ZOOM_KEY), '1000');
    await other.event('time-days', 'click');
  }
});

test('invalid zoom preferences fall back to five hundred blocks and blocked storage never disables zoom', async () => {
  for (const value of ['bad', 'NaN', 'Infinity', '-144', '0', '9', '500.5', String(Number.MAX_SAFE_INTEGER + 1), '{}']) {
    const storage = new Map([[ZOOM_KEY, value]]);
    const page = await openPage('/', storage);
    assert.match(page.elements.get('chart-window').textContent, /500 blocks/);
    assert.equal(storage.get(ZOOM_KEY), value, 'startup does not rewrite malformed storage');
    assert.equal(page.elements.get('zoom-in').disabled, false);
    assert.equal(page.elements.get('zoom-out').disabled, false);
  }
  const page = await openPage('/', { get() { throw new Error('Storage blocked'); }, set() { throw new Error('Storage blocked'); } });
  await page.event('zoom-in', 'click');
  assert.match(page.elements.get('chart-window').textContent, /250 blocks/);
  assert.equal(page.elements.get('zoom-in').disabled, false);
  assert.equal(page.elements.get('connection').textContent, 'Live');
  const legacy = new Map([['flame-mint-playground-zoom-v1', '4464']]);
  const fresh = await openPage('/', legacy);
  assert.match(fresh.elements.get('chart-window').textContent, /500 blocks/);
  assert.equal(legacy.get('flame-mint-playground-zoom-v1'), '4464', 'old linear preferences are left untouched');
  assert.equal(legacy.has(ZOOM_KEY), false);
});

test('arbitrary safe radii survive reloads and zooming stops only at the numeric precision ceiling', async () => {
  for (const radius of [10, 11, 143, 500, 4465, 1000000, 1000000000000, Number.MAX_SAFE_INTEGER]) {
    const page = await openPage('/', new Map([[ZOOM_KEY, String(radius)]]));
    assert.match(page.elements.get('chart-window').textContent, new RegExp(`${new Intl.NumberFormat('en').format(radius)} blocks`));
    assert.equal(page.elements.get('zoom-in').disabled, radius === 10);
    assert.equal(page.elements.get('zoom-out').disabled, radius === Number.MAX_SAFE_INTEGER);
    const lineCalls = page.draws.filter(call => ['moveTo', 'lineTo'].includes(call.method));
    assert.ok(lineCalls.length < 10000, 'wide views must not allocate or draw one sample per block');
    assert.ok(lineCalls.every(call => call.args.every(Number.isFinite)), `all canvas coordinates remain finite at radius ${radius}`);
    if (radius > 10) {
      await page.event('zoom-in', 'click');
      assert.equal(page.storage.get(ZOOM_KEY), String(Math.max(10, Math.round(radius / 2))));
    }
  }
  const page = await openPage('/', new Map([[ZOOM_KEY, String(Math.ceil(Number.MAX_SAFE_INTEGER / 2))]]));
  await page.event('zoom-out', 'click');
  assert.equal(page.storage.get(ZOOM_KEY), String(Number.MAX_SAFE_INTEGER));
  assert.equal(page.elements.get('zoom-out').disabled, true);
  await page.event('zoom-out', 'click');
  assert.equal(page.storage.get(ZOOM_KEY), String(Number.MAX_SAFE_INTEGER));
  await page.event('zoom-in', 'click');
  assert.equal(page.storage.get(ZOOM_KEY), String(Math.round(Number.MAX_SAFE_INTEGER / 2)));
  assert.equal(page.elements.get('zoom-out').disabled, false);
});

test('wide chart samples preserve starts, one-block spikes, remainder changes, and expiries without per-block work', () => {
  const height = 6000;
  const spike = plan({ start_block: 6011, duration: 1, budget_sats: FEE + 1 });
  const remainder = plan({ start_block: 7000, duration: 100, budget_sats: 300005 });
  const pending = plan({ start_block: 9000, duration: 144, budget_sats: 432001 });
  const started = performance.now();
  for (const radius of [1000000, 1000000000000, Number.MAX_SAFE_INTEGER]) {
    const first = height - radius, last = height + radius;
    const blocks = chartBlocks([spike, remainder], first, last, height, pending);
    assert.ok(blocks.length < 40, 'cost follows the number of schedule changes, not the view width');
    assert.deepEqual(blocks, [...new Set(blocks)].sort((a, b) => a - b));
    assert.ok(blocks.every(block => Number.isFinite(block) && Number.isInteger(block) && block >= first && block <= last));
    for (const block of [first, last, height, 6010, 6011, 6012, 6999, 7000, 7004, 7005, 7099, 7100, 8999, 9000, 9143, 9144]) {
      assert.ok(blocks.includes(block), `essential boundary ${block} survives radius ${radius}`);
    }
    assert.deepEqual(blocks.filter(block => feeAt(spike, block) > 0), [6011], 'a one-block fee spike remains represented');
    assert.equal(burnAt(remainder, 7004), 2001);
    assert.equal(burnAt(remainder, 7005), 2000);
  }
  assert.ok(performance.now() - started < 1000, 'huge views do not freeze the page');
  assert.deepEqual(chartBlocks([], -10, 10, 0), Array.from({ length: 21 }, (_, i) => i - 10), 'minimum zoom keeps every block and the negative-time blank range');
});

test('calendar zoom endpoints label the exact centered day range on both pages', async () => {
  for (const pathname of ['/', '/mint']) {
    for (const radius of [10, 500, 4464, 1000000]) {
      const page = await openPage(pathname, new Map([[ZOOM_KEY, String(radius)], [TIME_KEY, 'days']]));
      page.snapshot.height = 6000;
      page.draws.length = 0;
      await page.refresh();
      const ticks = page.draws.filter(call => call.canvas === page.elements.get('chart') && call.method === 'fillText' && call.args[2] === 267);
      assert.deepEqual(ticks.map(call => call.args[0]), Array.from({ length: 5 }, (_, index) => blockDate(Math.round(6000 - radius + radius / 2 * index))));
      assert.equal(ticks[2].args[1], 400);
      assert.match(page.elements.get('chart-window').textContent, new RegExp(`${new Intl.NumberFormat('en', { maximumFractionDigits: 3 }).format(radius / 144)} days`));
      assert.equal(page.elements.get('current-block').textContent, blockDate(6000));
    }
  }
});

test('zoom includes future fees and clips delayed draft traces at the selected boundary', async () => {
  for (const radius of [144, 720, 4464]) {
    const storage = new Map([[ZOOM_KEY, String(radius)]]);
    const dashboard = await openPage('/', storage);
    const first = dashboard.snapshot.contributions[0];
    dashboard.snapshot.height = 6000;
    dashboard.snapshot.contributions = [{ ...first, start_block: 6000 + radius - 1, duration: 144, budget_sats: 432000 }];
    dashboard.draws.length = 0;
    await dashboard.refresh();
    let path = [], fees;
    for (const call of dashboard.draws.filter(call => call.canvas === dashboard.elements.get('chart'))) {
      if (call.method === 'beginPath') path = [];
      if (call.method === 'moveTo' || call.method === 'lineTo') path.push(call.args);
      if (call.method === 'fill' && call.fillStyle?.kind === 'pattern') fees = path;
    }
    const baseline = fees[0][1];
    assert.equal(fees.filter(point => point[1] > baseline).length, 2, 'both final visible future blocks pay fees');
    const mint = await openPage('/mint', storage);
    mint.snapshot.height = 6000;
    mint.elements.get('duration').value = '144';
    await mint.refresh();
    const estimate = mint.elements.get('projected-reward').textContent;
    for (const [delay, visible] of [[radius - 1, true], [radius, false]]) {
      mint.elements.get('delay').value = String(delay);
      mint.draws.length = 0;
      await mint.event('delay', 'input');
      const draws = mint.draws.filter(call => call.canvas === mint.elements.get('chart'));
      const index = draws.findIndex(call => call.method === 'stroke' && call.strokeStyle === userColor(1));
      assert.equal(index >= 0, visible, 'the draft is visible only when a funded block is inside the window');
      if (visible) {
        const start = draws.slice(0, index).findLastIndex(call => call.method === 'beginPath');
        const trace = draws.slice(start + 1, index).filter(call => call.method === 'moveTo' || call.method === 'lineTo');
        assert.equal(trace.length, 2, 'the last visible funded block follows exactly one zero baseline sample');
        assert.equal(trace.at(-1).args[0], 752);
      }
      assert.equal(mint.elements.get('projected-reward').textContent, estimate, 'clipping never clips the full-duration estimate');
    }
  }
});

test('mint chart displays every known future schedule and expiry, with exact own overlays', async () => {
  const page = await openPage('/mint');
  const first = page.snapshot.contributions[0];
  page.snapshot.contributions = [
    { ...first, id: 1, user_id: 2, address: 'other', start_block: -89 },
    { ...first, id: 2, user_id: 3, address: 'other-future', start_block: 61, duration: 50, budget_sats: 150000 },
    { ...first, id: 3, user_id: 4, address: 'other-expired', start_block: -190 },
    { ...first, id: 4, start_block: 11, duration: 50, budget_sats: 150000 },
  ];
  page.draws.length = 0;
  await page.refresh();
  const paths = new Map();
  let path = [];
  for (const call of page.draws.filter(call => call.canvas === page.elements.get('chart'))) {
    if (call.method === 'beginPath') path = [];
    if (call.method === 'moveTo' || call.method === 'lineTo') path.push(call.args);
    if (call.method === 'fill') paths.set(call.fillStyle, path);
  }
  const thickness = (color, block) => {
    const index = block - (page.snapshot.height - DEFAULT_ZOOM);
    const path = paths.get(color);
    return path[2 * (2 * DEFAULT_ZOOM + 1) - index][1] - path[index + 1][1];
  };
  const otherHeight = thickness('#a3a3a3', 10);
  assert.ok(otherHeight > 0);
  for (const block of [11, 60, 111, 442]) assert.equal(thickness('#a3a3a3', block), 0, `no competing commitment is active at ${block}`);
  for (const block of [61, 110]) assert.equal(thickness('#a3a3a3', block), otherHeight, `the scheduled competitor is visible at ${block}`);
  assert.equal(thickness(userColor(1), 10), 0, 'own scheduled contribution has not started at now');
  assert.equal(thickness(userColor(1), 11), otherHeight, 'own concrete schedule begins next block');
  assert.equal(thickness(userColor(1), 60), otherHeight, 'own final funded block remains visible');
  assert.equal(thickness(userColor(1), 61), 0, 'own schedule expires rather than being extrapolated');
  assert.equal(thickness(userColor(1), 442), 0);
  assert.equal(page.elements.get('contributions').children.length, 1, 'only the wallet’s own schedule appears in its table');
  const draft = { budget_sats: 1000000, duration: 500, start_block: 11 };
  const estimate = projectedReward(page.snapshot.contributions, draft, first.address, 10);
  assert.equal(page.elements.get('projected-reward').textContent, new Intl.NumberFormat('en').format(Math.trunc(estimate / SPARKS)));
  assert.equal(page.elements.get('projected-rate').textContent, `${new Intl.NumberFormat('en').format(Math.trunc((draft.budget_sats - draft.duration * FEE) * SPARKS / estimate))} sat / FLM`);
});

test('dashboard includes scheduled contributions in future bands and shows their budgets and date ranges', async () => {
  const page = await openPage('/');
  const first = page.snapshot.contributions[0];
  page.snapshot.height = 144;
  page.snapshot.contributions = [{ ...first, id: 2, user_id: 2, address: 'scheduled-user', start_block: 145, duration: 144, budget_sats: 432000, burned_sats: '0', fees_sats: '0', reward_sparks: '0' }];
  page.draws.length = 0;
  await page.refresh();
  const row = page.elements.get('contributions').children[0];
  assert.equal(row.children[2].children[0].textContent, 'Scheduled at block 145');
  assert.equal(row.children[0].children[1].textContent, '432,000 sat / 144 blocks');
  let path = [];
  let band;
  for (const call of page.draws.filter(call => call.canvas === page.elements.get('chart'))) {
    if (call.method === 'beginPath') path = [];
    if (call.method === 'moveTo' || call.method === 'lineTo') path.push(call.args);
    if (call.method === 'fill' && call.fillStyle === userColor(2)) band = path;
  }
  const thickness = block => {
    const index = block - (page.snapshot.height - DEFAULT_ZOOM);
    return band[2 * (2 * DEFAULT_ZOOM + 1) - index][1] - band[index + 1][1];
  };
  assert.equal(thickness(144), 0);
  assert.ok(thickness(145) > 0, 'the next-block scheduled start is visible');
  assert.ok(thickness(288) > 0, 'the final funded block is visible');
  assert.equal(thickness(289), 0, 'the known expiry ends the band');
  const totals = page.elements.get('contribution-totals').children[0].children.map(cell => cell.textContent);
  await page.event('time-days', 'click');
  const dayRow = page.elements.get('contributions').children[0];
  assert.equal(dayRow.children[0].children[1].textContent, '432,000 sat / 1 day');
  assert.equal(dayRow.children[2].children[0].textContent, 'Scheduled at Jan 12');
  assert.deepEqual(page.elements.get('contribution-totals').children[0].children.map(cell => cell.textContent), totals, 'calendar display does not alter all-time totals');
});

test('dashboard burn and patterned downward fees share one sats scale, with mint rates on the right', async () => {
  const page = await openPage('/');
  const pattern = page.draws.find(call => call.method === 'fill' && call.fillStyle?.kind === 'pattern').fillStyle;
  const strokes = page.draws.filter(call => call.canvas === pattern.canvas);
  page.snapshot.height = 109;
  page.draws.length = 0;
  await page.refresh();
  const areaPath = (fillStyle, outermost = false) => {
    const draws = page.draws.filter(call => call.canvas === page.elements.get('chart'));
    const fill = draws[outermost ? 'findLastIndex' : 'findIndex'](call => call.method === 'fill' && fillStyle(call.fillStyle));
    assert.ok(fill >= 0, 'area drawn');
    const start = draws.slice(0, fill).findLastIndex(call => call.method === 'beginPath');
    return draws.slice(start + 1, fill).filter(call => call.method === 'moveTo' || call.method === 'lineTo');
  };
  const feePath = () => areaPath(style => style?.kind === 'pattern');
  const one = feePath();
  const baseline = one[0].args[1];
  assert.ok(one.every(call => call.args[1] >= baseline), 'fee area never extends above zero');
  assert.equal(one.filter(call => call.args[1] > baseline).length, 100, 'exactly the scheduled blocks have fees');
  const oneDepth = Math.max(...one.map(call => call.args[1])) - baseline;
  assert.ok(oneDepth > 0);
  const burnPath = areaPath(style => style === userColor(1));
  const burnHeight = baseline - Math.min(...burnPath.map(call => call.args[1]));
  assert.ok(Math.abs(burnHeight - 2 * oneDepth) < 1e-10, '2,000 sats burned and 1,000 sats in fees use identical pixels per sat');
  assert.equal(pattern.repetition, 'repeat');
  const from = strokes.find(call => call.method === 'moveTo').args;
  const to = strokes.find(call => call.method === 'lineTo').args;
  assert.ok(from[0] !== to[0] && from[1] !== to[1], 'fee tile contains diagonal strokes');
  assert.ok(strokes.some(call => call.method === 'stroke' && call.strokeStyle === '#888'), 'fee strokes are grey');
  const ticks = page.draws.filter(call => call.method === 'fillText');
  const numericTick = text => Number(String(text).replaceAll(',', '').replace('−', '-').replace(/k$/, 'e3').replace(/m$/, 'e6'));
  let positive = false, negative = false;
  for (const left of ticks.filter(call => call.args[1] < 50 && Number.isFinite(numericTick(call.args[0])))) {
    const right = ticks.find(call => call.args[1] > 750 && call.args[2] === left.args[2] && Number.isFinite(numericTick(call.args[0])));
    assert.ok(right, 'both vertical scales label the same horizontal tick');
    const sats = numericTick(left.args[0]), rate = numericTick(right.args[0]);
    assert.ok(Math.abs(sats - 50 * rate) <= Math.max(1, Math.abs(sats) * .01), 'right axis is left axis divided by 50 FLM');
    positive ||= sats > 0;
    negative ||= sats < 0;
  }
  assert.ok(positive && negative, 'both burn and fee regions have paired axis labels');
  assert.ok(page.draws.some(call => call.method === 'fillRect' && call.fillStyle === '#ffffff30'), 'the dashboard distinguishes its visible future region');
  page.snapshot.contributions.push({ ...page.snapshot.contributions[0], id: 2, budget_sats: 100100 });
  page.draws.length = 0;
  await page.refresh();
  const two = feePath();
  const newBaseline = two[0].args[1];
  const twoDepth = Math.max(...two.map(call => call.args[1])) - newBaseline;
  const twoBurnHeight = newBaseline - Math.min(...areaPath(style => PALETTE.includes(style), true).map(call => call.args[1]));
  const participantFills = page.draws.filter(call => call.method === 'fill' && PALETTE.includes(call.fillStyle));
  assert.equal(participantFills.length, 1, 'contributions from the same user share one chart band');
  assert.ok(participantFills.every(call => call.fillStyle === userColor(1)));
  assert.ok(participantFills.every(call => call.globalAlpha === 1), 'participant colors are opaque, not washed out');
  assert.ok(Math.abs(twoDepth / twoBurnHeight - 2000 / 2001) < 1e-10, 'overlapping same-address contributions pay two fees on the shared scale');
  page.snapshot.contributions = [];
  page.draws.length = 0;
  await page.refresh();
  const emptyFees = feePath();
  assert.ok(emptyFees.every(call => call.args[1] === emptyFees[0].args[1]), 'empty blocks have no fee area');
  const mint = await openPage('/mint');
  assert.equal(mint.elements.get('fees-legend').hidden, true);
  assert.equal(mint.elements.get('chart-wrap').className, 'canvas-wrap');
  assert.equal(mint.elements.get('chart-title').textContent, 'Burn rate');
  assert.ok(!mint.draws.some(call => call.method === 'fill' && call.fillStyle?.kind === 'pattern'));
  for (const left of mint.draws.filter(call => call.method === 'fillText' && call.args[1] < 50 && numericTick(call.args[0]) > 0)) {
    const right = mint.draws.find(call => call.method === 'fillText' && call.args[1] > 750 && call.args[2] === left.args[2]);
    assert.ok(right, 'mint page also has a right-hand mint-rate scale');
    assert.equal(numericTick(left.args[0]), 50 * numericTick(right.args[0]));
  }
});

test('dashboard initializes, polls, draws, and renders the actual contribution snapshot', async () => {
  const page = await openPage('/');
  assert.equal(page.elements.get('total-burned').textContent, '100,000\u00a0sat');
  assert.equal(page.elements.get('total-fees').textContent, '1,000\u00a0sat');
  assert.equal(page.elements.get('total-reward').textContent, '500\u00a0FLM');
  assert.equal(page.elements.get('total-rate').textContent, '40\u00a0sat/FLM');
  assert.equal(page.elements.get('current-block').textContent, '10');
  assert.equal(page.elements.get('connection').textContent, 'Live');
  assert.equal(page.elements.get('contributions').children.length, 1);
  assert.equal(page.elements.get('contributions').children[0].children[0].children[0].children[1].textContent, 'Minter 1 #1');
  assert.equal(page.elements.get('contributions').children[0].children[0].children[1].textContent, '300,000 sat / 100 blocks', 'public rows disclose the known budget and duration');
  assert.equal(page.requests.filter(request => request.url === '/api/state').length, 1);
  assert.equal(page.requests.length, 2, 'state and BTC/USD quote are loaded independently');
  assert.equal(page.storage.size, 0, 'dashboard does not need palette or wallet storage');
});

test('contribution rows show a name, budget and status without exposing addresses on the dashboard', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const contribution = page.snapshot.contributions[0];
    const address = contribution.address;
    const checkRow = () => {
      const row = page.elements.get('contributions').children[0];
      const heading = row.children[0].children[0].children[1];
      const details = row.children[0].children[1];
      assert.equal(heading.textContent, 'Minter 1 #1');
      assert.equal(details.textContent, '300,000 sat / 100 blocks');
      assert.equal(details.title, pathname === '/mint' ? address : '');
      assert.equal(page.snapshot.contributions[0].address, address, 'API address data remains available for ownership matching');
    };
    checkRow();
    await page.event('time-days', 'click');
    const row = page.elements.get('contributions').children[0];
    assert.equal(row.children[0].children[1].textContent, '300,000 sat / 0.694 days');
    assert.equal(row.children[0].children[1].title, pathname === '/mint' ? address : '');
    assert.equal(row.children[0].children[0].children[1].textContent, 'Minter 1 #1');
  }
});

test('contribution status reports the exact schedule and floors progress by coins burned, not elapsed blocks', async () => {
  const page = await openPage('/');
  const contribution = page.snapshot.contributions[0];
  page.snapshot.height = 59;
  contribution.budget_sats = 300005;
  contribution.burned_sats = '100002';
  const status = () => page.elements.get('contributions').children[0].children[2].children[0].textContent;
  await page.refresh();
  assert.equal(status(), 'Active 49%');
  contribution.burned_sats = '100003';
  await page.refresh();
  assert.equal(status(), 'Active 50%');
  contribution.start_block = 145;
  await page.refresh();
  assert.equal(status(), 'Scheduled at block 145');
  await page.event('time-days', 'click');
  assert.equal(status(), 'Scheduled at Jan 12');
  contribution.start_block = 10;
  page.snapshot.height = 109;
  await page.refresh();
  assert.equal(status(), 'Finished on Jan 11');
  await page.event('time-blocks', 'click');
  assert.equal(status(), 'Finished on block 109');
});

test('contribution identity and minter names remain distinct plain text for delegating users', async () => {
  const page = await openPage('/');
  const contribution = page.snapshot.contributions[0];
  contribution.mode = 'delegating';
  contribution.user_name = '<Alice & Bob>';
  contribution.minter_name = '<Service & co>';
  contribution.minter_id = 2;
  await page.refresh();
  const row = page.elements.get('contributions').children[0];
  assert.equal(row.children[0].children[0].children[1].textContent, '<Alice & Bob> #1');
  assert.equal(row.children[0].children[1].textContent, '300,000 sat / 100 blocks');
  assert.equal(row.children[1].textContent, '<Service & co>');
  assert.equal(row.children[0].children[0].children[0].style.background, BLUE_PALETTE[1]);
  assert.equal(row.children[0].children[1].title, '');
});

test('service and delegating personal totals conserve net rewards without claiming the delegate’s burned coins', async () => {
  const service = { id: 1, user_id: 1, user_name: 'Service 1', minter_id: 1, minter_name: 'Service 1', address: 'tf1demo-address', mode: 'service', commission_bps: 200, service_user_id: null, budget_sats: 300000, duration: 100, start_block: 10, burned_sats: '200000', fees_sats: '100000', reward_sparks: String(2500 * SPARKS), commission_sparks: '0' };
  const delegated = { id: 2, user_id: 2, user_name: 'Delegate 2', minter_id: 1, minter_name: 'Service 1', address: 'delegator', mode: 'delegating', commission_bps: 200, service_user_id: 1, budget_sats: 200000, duration: 100, start_block: 10, burned_sats: '200000', fees_sats: '0', reward_sparks: String(2450 * SPARKS), commission_sparks: String(50 * SPARKS) };
  const accounts = [
    { user_id: 1, address: service.address, name: 'Service 1', mode: 'service', commission_bps: 200, contribution_count: 1, burned_sats: '200000', fees_sats: '100000', reward_sparks: String(2550 * SPARKS) },
    { user_id: 2, address: delegated.address, name: 'Delegate 2', mode: 'delegating', commission_bps: 200, contribution_count: 1, burned_sats: '200000', fees_sats: '0', reward_sparks: String(2450 * SPARKS) },
  ];
  const setup = async page => {
    page.snapshot.height = 109;
    page.snapshot.contributions = [{ ...service }, { ...delegated }];
    page.snapshot.minters = accounts.map(account => ({ ...account }));
    page.snapshot.services = [{ user_id: 1, name: 'Service 1', commission_bps: 200, active: false }];
    page.snapshot.totals = { contribution_count: 2, burned_sats: '400000', fees_sats: '100000', reward_sparks: String(5000 * SPARKS) };
    page.draws.length = 0;
    await page.refresh();
  };
  const provider = await openPage('/mint');
  await setup(provider);
  assert.equal(provider.elements.get('total-burned').textContent, '200,000\u00a0sat');
  assert.equal(provider.elements.get('total-fees').textContent, '100,000\u00a0sat');
  assert.equal(provider.elements.get('total-reward').textContent, '2,550\u00a0FLM');
  assert.equal(provider.elements.get('total-rate').textContent, '78\u00a0sat/FLM');
  assert.equal(provider.elements.get('contributions').children.length, 2, 'the service can inspect its linked delegating contribution');
  const linked = provider.elements.get('contributions').children[0];
  assert.equal(linked.children[0].children[0].children[1].textContent, 'Delegate 2 #2');
  assert.equal(linked.children[0].children[0].children[0].style.background, BLUE_PALETTE[2]);
  assert.deepEqual(linked.children.slice(3).map(cell => cell.textContent), ['0', '50', '0', '0'], 'linked rows show only commission income, never the delegate’s costs or net reward');
  assert.deepEqual(footerValues(provider), ['200,000', '2,550', '100,000', '78']);
  assert.ok(provider.draws.some(call => call.method === 'fill' && call.fillStyle === BLUE_PALETTE[2]), 'the service chart exposes linked delegation in blue');
  const delegate = await openPage('/mint', new Map(), undefined, undefined, { walletAddress: () => delegated.address, profile: () => profileResponse('Delegate 2', 2) });
  await setup(delegate);
  assert.equal(delegate.elements.get('contributions').children.length, 1, 'delegators do not acquire the service’s own contribution');
  assert.equal(delegate.elements.get('total-burned').textContent, '200,000\u00a0sat');
  assert.equal(delegate.elements.get('total-fees').textContent, '0\u00a0sat');
  assert.equal(delegate.elements.get('total-reward').textContent, '2,450\u00a0FLM');
  assert.equal(delegate.elements.get('total-rate').textContent, '81\u00a0sat/FLM');
  assert.deepEqual(rowValues(delegate), ['200,000', '2,450', '0', '81']);
  const dashboard = await openPage('/');
  await setup(dashboard);
  assert.equal(dashboard.elements.get('total-reward').textContent, '5,000\u00a0FLM');
  assert.equal(accounts.reduce((sum, account) => sum + Number(account.reward_sparks), 0), Number(dashboard.snapshot.totals.reward_sparks));
  for (const page of [provider, delegate, dashboard]) {
    const oldDelegation = page.snapshot.contributions.find(contribution => contribution.id === 2);
    oldDelegation.commission_bps = 5000;
    oldDelegation.reward_sparks = String(1250 * SPARKS);
    oldDelegation.commission_sparks = String(1250 * SPARKS);
    page.snapshot.services[0].commission_bps = 5000;
    page.snapshot.minters[0].commission_bps = 5000;
    page.snapshot.minters[0].reward_sparks = String(3750 * SPARKS);
    page.snapshot.minters[1].commission_bps = 5000;
    page.snapshot.minters[1].reward_sparks = String(1250 * SPARKS);
    await page.refresh();
  }
  assert.equal(provider.elements.get('total-reward').textContent, '3,750\u00a0FLM');
  assert.equal(provider.elements.get('total-rate').textContent, '53\u00a0sat/FLM');
  assert.deepEqual(rowValues(provider), ['0', '1,250', '0', '0'], 'linked rows show the commission the server accrued');
  assert.equal(delegate.elements.get('total-reward').textContent, '1,250\u00a0FLM');
  assert.equal(delegate.elements.get('total-rate').textContent, '160\u00a0sat/FLM');
  assert.deepEqual(rowValues(delegate), ['200,000', '1,250', '0', '160']);
  assert.equal(dashboard.elements.get('total-reward').textContent, '5,000\u00a0FLM', 'commission splits do not create or destroy global rewards');
});

test('dashboard minter totals sort by raw numeric values, preserve modes and colors, and do not mutate the snapshot', async () => {
  const page = await openPage('/');
  const alice = { user_id: 1, address: 'alice', name: 'Alice', mode: 'independent', commission_bps: 0, contribution_count: 1, burned_sats: '200', reward_sparks: String(1000 * SPARKS), fees_sats: '2' };
  const bob = { user_id: 2, address: 'bob', name: 'Bob', mode: 'service', commission_bps: 200, contribution_count: 1, burned_sats: '100', reward_sparks: String(201 * SPARKS), fees_sats: '10001' };
  const zebra = { user_id: 3, address: 'zebra', name: 'Zebra', mode: 'delegating', commission_bps: 200, contribution_count: 1, burned_sats: '9', reward_sparks: String(SPARKS), fees_sats: '900' };
  page.snapshot.minters = [zebra, alice, bob];
  await page.refresh();
  const rows = () => page.elements.get('minters').children;
  const names = () => rows().map(row => row.children[0].children[0].children[1].textContent.replace(/\s*·?\s*2%$/, ''));
  assert.equal(page.elements.get('minters-panel').hidden, false);
  assert.deepEqual(names(), ['Alice', 'Bob', 'Zebra']);
  assert.equal(rows()[0].children[0].children[0].children[0].style.background, userColor(1));
  assert.equal(rows()[2].children[0].children[0].children[0].style.background, BLUE_PALETTE[3]);
  const text = node => `${node.textContent ?? ''}${node.children.map(text).join('')}`;
  assert.match(text(rows()[1].children[0]), /2%/, 'service commission appears next to its name, not in a separate column');
  assert.ok(rows().every(row => row.children.length === 5));
  await page.event('sort-name', 'click');
  assert.deepEqual(names(), ['Zebra', 'Bob', 'Alice']);
  for (const [id, expected] of [
    ['burned_sats', ['Alice', 'Bob', 'Zebra']],
    ['reward_sparks', ['Alice', 'Bob', 'Zebra']],
    ['fees_sats', ['Bob', 'Zebra', 'Alice']],
    ['rate', ['Zebra', 'Bob', 'Alice']],
  ]) {
    await page.event(`sort-${id}`, 'click');
    assert.deepEqual(names(), expected, `descending ${id}`);
    await page.event(`sort-${id}`, 'click');
    assert.deepEqual(names(), [...expected].reverse(), `ascending ${id}`);
  }
  assert.deepEqual(page.snapshot.minters.map(minter => minter.user_id), [3, 1, 2]);
  await page.event('unit-btc', 'click');
  assert.deepEqual(names(), ['Alice', 'Bob', 'Zebra'], 'currency changes do not change raw mint-rate sort order');
  bob.name = '<b>Bob</b>';
  await page.refresh();
  assert.ok(rows().some(row => row.children[0].children[0].children[1].textContent.includes('<b>Bob</b>')), 'untrusted names remain text');
  const mint = await openPage('/mint');
  assert.equal(mint.elements.get('minters-panel').hidden, true);
});

test('self-delegation appears once and reunites its net reward and commission in the owner’s balance', async () => {
  const page = await openPage('/mint');
  const first = page.snapshot.contributions[0];
  page.snapshot.height = 109;
  page.snapshot.contributions = [
    { ...first, mode: 'service', commission_bps: 200, burned_sats: '200000', fees_sats: '100000', reward_sparks: String(2500 * SPARKS) },
    { ...first, id: 2, mode: 'delegating', service_user_id: 1, minter_id: 1, commission_bps: 200, budget_sats: 200000, burned_sats: '200000', fees_sats: '0', reward_sparks: String(2450 * SPARKS), commission_sparks: String(50 * SPARKS) },
  ];
  page.snapshot.minters = [{ user_id: 1, address: first.address, name: 'Minter 1', mode: 'delegating', commission_bps: 200, contribution_count: 2, burned_sats: '400000', fees_sats: '100000', reward_sparks: String(5000 * SPARKS) }];
  await page.refresh();
  assert.equal(page.elements.get('contributions').children.length, 2);
  assert.deepEqual(rowValues(page), ['200,000', '2,500', '0', '80']);
  assert.deepEqual(footerValues(page), ['400,000', '5,000', '100,000', '80']);
  assert.equal(page.elements.get('total-reward').textContent, '5,000\u00a0FLM');
});

test('dashboard uses server-supplied latest contributions and all-time totals, even with no visible rows', async () => {
  const page = await openPage('/');
  page.snapshot.totals = { contribution_count: 42, burned_sats: '9876543', fees_sats: '1234567', reward_sparks: '123456789999' };
  await page.refresh();
  const footer = () => page.elements.get('contribution-totals').children[0].children.map(cell => cell.textContent);
  assert.equal(page.elements.get('contributions').children.length, 1, 'only the returned detailed row is displayed');
  assert.deepEqual(footer(), ['All-time total', '', '42 contributions', '9,876,543', '1,234', '1,234,567', '7,999']);
  assert.equal(page.elements.get('total-burned').textContent, '9,876,543\u00a0sat');
  assert.equal(page.elements.get('total-fees').textContent, '1,234,567\u00a0sat');
  assert.equal(page.elements.get('total-reward').textContent, '1,234\u00a0FLM');
  page.snapshot.contributions = [];
  page.draws.length = 0;
  await page.refresh();
  assert.equal(page.elements.get('contributions').children.length, 0);
  assert.equal(page.elements.get('empty-table').hidden, false);
  assert.equal(page.elements.get('empty-table').textContent, 'No contributions.');
  assert.ok(!page.draws.some(call => call.method === 'fill' && PALETTE.includes(call.fillStyle)), 'omitted contributions create no chart bands');
  assert.deepEqual(footer(), ['All-time total', '', '42 contributions', '9,876,543', '1,234', '1,234,567', '7,999']);
  assert.ok(page.requests.filter(request => request.url.startsWith('/api/state')).every(request => request.url === '/api/state'), 'dashboard never requests full history');
});

test('dashboard renders the server’s latest two hundred rows without an age cutoff or client-side limit', async () => {
  const page = await openPage('/');
  const first = page.snapshot.contributions[0];
  page.snapshot.height = 100000;
  page.snapshot.totals = { contribution_count: 12345, burned_sats: '9876543', fees_sats: '1234567', reward_sparks: '123456789999' };
  page.snapshot.contributions = Array.from({ length: 200 }, (_, index) => ({ ...first, id: 600 - index, start_block: 1 }));
  await page.refresh();
  const rows = () => page.elements.get('contributions').children;
  const identifier = row => row.children[0].children[0].children[1].textContent.match(/#\d+$/)[0];
  assert.equal(rows().length, 200);
  assert.equal(identifier(rows()[0]), '#600');
  assert.equal(identifier(rows().at(-1)), '#401');
  assert.ok(rows().every(row => row.children[2].children[0].textContent === 'Finished on block 100'), 'old returned rows are not filtered by block age');
  assert.deepEqual(page.elements.get('contribution-totals').children[0].children.map(cell => cell.textContent), ['All-time total', '', '12345 contributions', '9,876,543', '1,234', '1,234,567', '7,999']);
  page.snapshot.contributions.push({ ...first, id: 400, start_block: 1 });
  await page.refresh();
  assert.equal(rows().length, 201, 'the database, not the browser, owns the response limit');
  assert.equal(identifier(rows().at(-1)), '#400');
  assert.ok(page.requests.filter(request => request.url.startsWith('/api/state')).every(request => request.url === '/api/state'));
});

test('lightweight chart schedules preserve omitted old active contributions and the current mint rate', async () => {
  const page = await openPage('/');
  const first = page.snapshot.contributions[0];
  page.snapshot.height = 20000;
  const detailed = { ...first, id: 600, user_id: 3, start_block: 20001 };
  const old = { id: 1, user_id: 2, start_block: 1, duration: 50000, budget_sats: 150000000 };
  const { id, user_id, start_block, duration, budget_sats } = detailed;
  page.snapshot.contributions = [detailed];
  page.snapshot.chart_contributions = [old, { id, user_id, start_block, duration, budget_sats }];
  page.draws.length = 0;
  await page.refresh();
  assert.equal(page.elements.get('contributions').children.length, 1, 'chart-only schedules do not create detailed table rows');
  assert.equal(page.elements.get('contributions').children[0].children[0].children[0].children[1].textContent, 'Minter 3 #600');
  assert.equal(page.elements.get('total-rate').textContent, '40\u00a0sat/FLM', 'the old contribution still burns 2,000 sats in the current block');
  assert.ok(page.draws.some(call => call.method === 'fill' && call.fillStyle === userColor(old.user_id)), 'the old active user still has a chart band');
  assert.equal(page.elements.get('total-burned').textContent, '100,000\u00a0sat', 'all-time counters still come from server totals');
  page.snapshot.height = 20001;
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '80\u00a0sat/FLM', 'all current schedules, including the new detailed row, contribute once');
  page.snapshot.chart_contributions = [];
  page.draws.length = 0;
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '—\u00a0sat/FLM', 'an explicitly empty chart list is authoritative');
  assert.ok(!page.draws.some(call => call.method === 'fill' && PALETTE.includes(call.fillStyle)));
  assert.equal(page.elements.get('contributions').children.length, 1, 'the detailed list remains independent');
});

test('orphaned delegated burns remain blue on charts but create no fees or fictitious current mint rate', async () => {
  const page = await openPage('/');
  const first = page.snapshot.contributions[0];
  const orphan = { ...first, user_id: 2, mode: 'delegating', service_user_id: 99, commission_bps: 200, budget_sats: 300000 };
  page.snapshot.height = 20;
  page.snapshot.contributions = [orphan];
  page.draws.length = 0;
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '—\u00a0sat/FLM');
  assert.ok(page.draws.some(call => call.method === 'fill' && call.fillStyle === BLUE_PALETTE[2]));
  let path = [], fees;
  for (const call of page.draws.filter(call => call.canvas === page.elements.get('chart'))) {
    if (call.method === 'beginPath') path = [];
    if (call.method === 'moveTo' || call.method === 'lineTo') path.push(call.args);
    if (call.method === 'fill' && call.fillStyle?.kind === 'pattern') fees = path;
  }
  assert.ok(fees.every(point => point[1] === fees[0][1]), 'orphaned delegations never fabricate mining fees');
  page.snapshot.contributions.push({ ...first, id: 2, user_id: 3 });
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '100\u00a0sat/FLM', 'the factual rate includes 3,000 wasted plus 2,000 eligible sats burned for 50 FLM');
});

test('mint requests full history and retains personal totals for contributions finished more than 10,000 blocks ago', async () => {
  const page = await openPage('/mint');
  page.snapshot.height = 20000;
  page.snapshot.contributions[0].reward_sparks = '199999999';
  assert.equal(Object.hasOwn(page.snapshot, 'chart_contributions'), false, 'full-history responses do not need a duplicate chart list');
  await page.refresh();
  assert.ok(page.requests.filter(request => request.url.startsWith('/api/state')).every(request => request.url === '/api/state?full_history=true'));
  assert.equal(page.elements.get('contributions').children.length, 1);
  assert.equal(page.elements.get('contributions').children[0].children[2].children[0].textContent, 'Finished on block 109');
  assert.equal(page.elements.get('total-burned').textContent, '2,000\u00a0sat');
  assert.equal(page.elements.get('total-reward').textContent, '1\u00a0FLM');
  assert.equal(page.elements.get('total-fees').textContent, '1,000\u00a0sat');
  assert.equal(page.elements.get('total-rate').textContent, '1,000\u00a0sat/FLM');
  assert.deepEqual(page.elements.get('contribution-totals').children[0].children.map(cell => cell.textContent), ['Total', '', '1 contributions', '2,000', '1', '1,000', '1,000']);
});

test('FLM and sat/FLM displays truncate fractions without changing rewards or projection calculations', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const contribution = page.snapshot.contributions[0];
    contribution.burned_sats = '1000'; contribution.fees_sats = '999'; contribution.reward_sparks = '199999999';
    contribution.budget_sats = 300005;
    page.snapshot.totals = { contribution_count: 1, burned_sats: '1000', fees_sats: '999', reward_sparks: '199999999' };
    const before = JSON.stringify(page.snapshot);
    await page.refresh();
    const row = page.elements.get('contributions').children[0].children.map(cell => cell.textContent);
    const footer = page.elements.get('contribution-totals').children[0].children.map(cell => cell.textContent);
    assert.deepEqual(row.slice(3), ['1,000', '1', '999', '500']);
    assert.deepEqual(footer.slice(3), ['1,000', '1', '999', '500']);
    assert.equal(page.elements.get('total-reward').textContent, '1\u00a0FLM');
    assert.equal(page.elements.get('total-fees').textContent, '999\u00a0sat');
    assert.equal(page.elements.get('total-rate').textContent, `${pathname === '/mint' ? '500' : '40'}\u00a0sat/FLM`);
    if (pathname === '/mint') {
      const draft = { budget_sats: 1000000, duration: 500, start_block: page.snapshot.height + 1 };
      const estimate = projectedReward(page.snapshot.contributions, draft, 'tf1demo-address');
      assert.notEqual(estimate / SPARKS, Math.trunc(estimate / SPARKS));
      assert.equal(page.elements.get('projected-reward').textContent, new Intl.NumberFormat('en').format(Math.trunc(estimate / SPARKS)));
      assert.equal(page.elements.get('projected-rate').textContent, `${Math.trunc((draft.budget_sats - draft.duration * FEE) * SPARKS / estimate)} sat / FLM`);
    }
    assert.equal(JSON.stringify(page.snapshot), before, 'display formatting never changes stored or received quantities');
    contribution.burned_sats = '7'; contribution.fees_sats = '0'; contribution.reward_sparks = '7000000';
    await page.refresh();
    assert.deepEqual(page.elements.get('contributions').children[0].children.slice(3).map(cell => cell.textContent), ['7', '0', '0', '100'], 'an exact integer rate does not lose one to floating-point division by FLM');
  }
});

test('mint-rate chart ticks truncate fractional rates, leaving the satoshi scale unchanged', async () => {
  const page = await openPage('/mint');
  page.snapshot.contributions[0].budget_sats = 100100;
  page.elements.get('budget').value = '100000';
  page.draws.length = 0;
  await page.refresh();
  const ticks = page.draws.filter(call => call.canvas === page.elements.get('chart') && call.method === 'fillText');
  const left = ticks.find(call => call.args[0] === '1' && call.args[1] < 50);
  assert.ok(left, 'the left axis still labels one satoshi');
  const right = ticks.find(call => call.args[1] > 750 && call.args[2] === left.args[2]);
  assert.equal(right.args[0], '0', '0.02 sat/FLM truncates only in the displayed axis label');
});

test('mint totals separate accrued mining fees and calculate effective rate from burns alone', async () => {
  const page = await openPage('/mint');
  const first = page.snapshot.contributions[0];
  page.snapshot.height = 250;
  page.snapshot.contributions = [
    { ...first, id: 1, start_block: 10, burned_sats: '110000', fees_sats: '100000', reward_sparks: '12345678901' },
    { ...first, id: 3, start_block: 200, burned_sats: '12345', fees_sats: '17000', reward_sparks: '987654321' },
    { ...first, id: 4, start_block: 300, budget_sats: 100000000, burned_sats: '0', fees_sats: '0', reward_sparks: '0' },
    { ...first, id: 5, user_id: 2, address: 'another-user', burned_sats: '999999', fees_sats: '999999', reward_sparks: '999999999999' },
  ];
  await page.refresh();
  const totals = () => ['total-burned', 'total-fees', 'total-reward', 'total-rate'].map(id => page.elements.get(id).textContent);
  assert.deepEqual(totals(), ['122,345\u00a0sat', '117,000\u00a0sat', '133\u00a0FLM', '917\u00a0sat/FLM']);
  assert.equal(page.elements.get('total-rate-label').textContent, 'Effective rate');
  assert.match(page.elements.get('total-rate-label').title, /excluding.*fees/i);
  const table = page.elements.get('contribution-totals').children[0].children.map(cell => cell.textContent);
  assert.deepEqual(table.slice(3), ['122,345', '133', '117,000', '917'], 'header and table truncate only displayed reward and burn-only rate');
  assert.deepEqual(page.elements.get('contributions').children.map(row => row.children[2].children[0].textContent.split(' ')[0]), ['Scheduled', 'Active', 'Finished']);
  page.elements.get('budget').value = String(BUDGET_MAX);
  await page.event('budget', 'input');
  page.elements.get('duration').value = '1000';
  await page.event('duration', 'input');
  assert.deepEqual(totals(), ['122,345\u00a0sat', '117,000\u00a0sat', '133\u00a0FLM', '917\u00a0sat/FLM'], 'draft budgets and projected rewards never change the accrued header');
});

test('changing accrued fees changes fee displays but never burn-based mint rates or draft estimates', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const rates = () => [page.elements.get('total-rate').textContent, rowValues(page).at(-1), footerValues(page).at(-1), page.elements.get('projected-rate').textContent];
    const before = rates();
    page.snapshot.totals.fees_sats = '999999';
    page.snapshot.contributions[0].fees_sats = '999999';
    await page.refresh();
    assert.equal(page.elements.get('total-fees').textContent, '999,999\u00a0sat');
    assert.equal(rowValues(page)[2], '999,999');
    assert.equal(footerValues(page)[2], '999,999');
    assert.deepEqual(rates(), before, `${pathname}: fees must not be counted as burned coins`);
  }
});

test('mint effective cost is undefined before any FLM is earned, and a ready unregistered wallet has zero totals', async () => {
  const page = await openPage('/mint');
  page.snapshot.contributions[0].reward_sparks = '0';
  await page.refresh();
  assert.equal(page.elements.get('total-burned').textContent, '2,000\u00a0sat');
  assert.equal(page.elements.get('total-fees').textContent, '1,000\u00a0sat');
  assert.equal(page.elements.get('total-reward').textContent, '0\u00a0FLM');
  assert.equal(page.elements.get('total-rate').textContent, '—\u00a0sat/FLM');
  page.snapshot.contributions[0].address = 'another-user';
  await page.refresh();
  assert.equal(page.elements.get('total-burned').textContent, '0\u00a0sat');
  assert.equal(page.elements.get('total-fees').textContent, '0\u00a0sat');
  assert.equal(page.elements.get('total-reward').textContent, '0\u00a0FLM');
  assert.equal(page.elements.get('total-rate').textContent, '—\u00a0sat/FLM');
});

test('an unavailable wallet never substitutes global totals on the mint page', async () => {
  const storage = new Map([['flame-mint-playground-wallet-v1', '{broken json']]);
  const page = await openPage('/mint', storage);
  assert.deepEqual(['total-burned', 'total-fees', 'total-reward', 'total-rate'].map(id => page.elements.get(id).textContent), ['—\u00a0sat', '—\u00a0sat', '—\u00a0FLM', '—\u00a0sat/FLM']);
  assert.equal(page.elements.get('projected-reward').textContent, '—', 'an unknown identity cannot distinguish its own future schedules');
  assert.equal(page.elements.get('projected-rate').textContent, '—');
  await page.refresh();
  assert.deepEqual(['total-burned', 'total-fees', 'total-reward', 'total-rate'].map(id => page.elements.get(id).textContent), ['—\u00a0sat', '—\u00a0sat', '—\u00a0FLM', '—\u00a0sat/FLM']);
  assert.equal(page.elements.get('contributions').children.length, 0);
});

test('a wallet arriving after the first poll fills personal totals without waiting for another poll', async () => {
  let finishWallet;
  const walletReady = new Promise(resolve => { finishWallet = resolve; });
  const page = await openPage('/mint', new Map(), undefined, walletReady);
  assert.equal(page.elements.get('connection').textContent, 'Live', 'the snapshot arrived while wallet initialization is pending');
  assert.deepEqual(['total-burned', 'total-fees', 'total-reward', 'total-rate'].map(id => page.elements.get(id).textContent), ['—\u00a0sat', '—\u00a0sat', '—\u00a0FLM', '—\u00a0sat/FLM']);
  finishWallet();
  await page.settle();
  assert.equal(page.elements.get('total-burned').textContent, '2,000\u00a0sat');
  assert.equal(page.elements.get('total-reward').textContent, '50\u00a0FLM');
  assert.equal(page.elements.get('total-fees').textContent, '1,000\u00a0sat');
  assert.equal(page.elements.get('total-rate').textContent, '40\u00a0sat/FLM');
  assert.equal(page.requests.filter(request => request.url === '/api/state?full_history=true').length, 1, 'wallet completion reuses the current full-history snapshot');
});

test('dashboard groups contributions by user ID, with the most recent contributor on top', async () => {
  const page = await openPage('/');
  const initialFills = page.draws.filter(call => call.method === 'fill' && PALETTE.includes(call.fillStyle));
  assert.deepEqual(initialFills.map(call => call.fillStyle), [userColor(1)]);
  assert.ok(initialFills.every(call => call.globalAlpha === 1), 'colors are opaque from the initial draw');
  const first = page.snapshot.contributions[0];
  page.snapshot.contributions.push(
    { ...first, id: 2, user_id: 26, address: 'next-user' },
    { ...first, id: 3 },
  );
  page.draws.length = 0;
  await page.refresh();
  const chartColors = page.draws.filter(call => call.method === 'fill' && PALETTE.includes(call.fillStyle)).map(call => call.fillStyle);
  assert.deepEqual(chartColors, [userColor(26), userColor(1)]);
  const tableColors = page.elements.get('contributions').children.map(row => row.children[0].children[0].children[0].style.background);
  assert.deepEqual(tableColors, [userColor(1), userColor(26), userColor(1)]);
});

test('dashboard sums each user’s burns and moves their complete band on top when they contribute again', async () => {
  const page = await openPage('/');
  page.snapshot.height = 12;
  const first = page.snapshot.contributions[0];
  const contributions = [
    { ...first, id: 1, budget_sats: 220000, start_block: 10 },
    { ...first, id: 2, user_id: 26, address: 'another-user', budget_sats: 430000, start_block: 8 },
    { ...first, id: 3, budget_sats: 760000, start_block: 9 },
  ];
  const bands = (page, colors = PALETTE, currentX = 400) => {
    let path = [];
    const result = [];
    for (const call of page.draws.filter(call => call.canvas === page.elements.get('chart'))) {
      if (call.method === 'beginPath') path = [];
      if (call.method === 'moveTo' || call.method === 'lineTo') path.push(call.args);
      if (call.method === 'fill' && colors.includes(call.fillStyle)) {
        const [top, base] = path.filter(point => point[0] === currentX).map(point => point[1]);
        assert.ok(Number.isFinite(top) && Number.isFinite(base));
        result.push({ color: call.fillStyle, top, base });
      }
    }
    return result;
  };
  const redraw = async (page, contributions) => {
    page.snapshot.contributions = contributions;
    page.draws.length = 0;
    await page.refresh();
  };
  const close = (actual, expected) => assert.ok(Math.abs(actual - expected) < 1e-10, `${actual} differs from ${expected}`);
  const reversed = contributions.slice().reverse();
  await redraw(page, reversed);
  assert.deepEqual(reversed.map(contribution => contribution.id), [3, 2, 1], 'drawing does not mutate the snapshot');
  const initial = bands(page);
  assert.deepEqual(initial.map(band => band.color), [userColor(26), userColor(1)]);
  const pixelsPerSat = (initial[0].base - initial[0].top) / burnAt(contributions[1], 12);
  let total = 0;
  for (const [index, burn] of [3300, 7800].entries()) {
    close(initial[index].base, initial[0].base - total * pixelsPerSat);
    total += burn;
    close(initial[index].top, initial[0].base - total * pixelsPerSat);
    assert.ok(initial[index].top < initial[index].base, 'each user has a positive band for their combined burns');
  }
  assert.deepEqual(page.elements.get('contributions').children.map(row => row.children[0].children[0].children[1].textContent.match(/#\d+$/)[0]), ['#3', '#2', '#1'], 'table remains newest first');
  await redraw(page, [contributions[1], contributions[0], contributions[2]]);
  assert.deepEqual(bands(page), initial, 'input order has no effect on band order or geometry');
  const latest = { ...first, id: 4, user_id: 26, address: 'another-user', budget_sats: 110000, start_block: 11 };
  await redraw(page, [latest, ...reversed]);
  const added = bands(page);
  assert.deepEqual(added.map(band => band.color), [userColor(1), userColor(26)], 'the returning user moves above the other user without changing color');
  close(added[0].base, initial[0].base);
  close(added[0].base - added[0].top, 7800 * pixelsPerSat);
  close(added[1].base, added[0].top);
  close(added[1].base - added[1].top, 3400 * pixelsPerSat);
  await redraw(page, [...contributions, latest]);
  assert.deepEqual(bands(page), added, 'the latest contribution ID determines user order, not snapshot order');
  await redraw(page, [{ ...latest, id: 5, user_id: 13, address: 'same-color-other-user' }, ...contributions, latest]);
  assert.deepEqual(bands(page).map(band => band.color), [userColor(1), userColor(26), userColor(13)], 'distinct users sharing a palette color remain separate bands');

  const mint = await openPage('/mint');
  mint.snapshot.height = 12;
  await redraw(mint, reversed);
  const ownAndOthers = bands(mint, ['#a3a3a3', userColor(1)], 400);
  assert.deepEqual(ownAndOthers.map(band => band.color), ['#a3a3a3', userColor(1)], 'mint page still groups others below all of the wallet’s contributions');
  close(ownAndOthers[1].base, ownAndOthers[0].top);
  close((ownAndOthers[1].base - ownAndOthers[1].top) / (ownAndOthers[0].base - ownAndOthers[0].top), 7800 / 3300);
});

test('mint chart, draft, legend, and table use the wallet user ID while other users stay grey', async () => {
  const page = await openPage('/mint');
  const first = page.snapshot.contributions[0];
  for (const user_id of [4, 26]) {
    page.snapshot.contributions = [
      { ...first, id: 3, user_id, address: first.address },
      { ...first, id: 2, user_id: 5, address: 'another-user' },
      { ...first, id: 1, user_id, address: first.address },
    ];
    page.draws.length = 0;
    await page.refresh();
    const color = userColor(user_id);
    const draws = page.draws.filter(call => call.canvas === page.elements.get('chart'));
    const fills = draws.filter(call => call.method === 'fill');
    assert.deepEqual(fills.map(call => call.fillStyle), ['#a3a3a3', color], 'other users remain grey regardless of their assigned palette color');
    assert.equal(fills[0].globalAlpha, .76, 'other users keep their existing grey appearance');
    assert.equal(fills[1].globalAlpha, 1, 'the wallet color is not faded');
    assert.ok(draws.some(call => call.method === 'stroke' && call.strokeStyle === color), 'draft line matches the assigned user color');
    assert.equal(page.elements.get('own-swatch').style.background, color);
    assert.equal(page.elements.get('draft-swatch').style.borderColor, color);
    const tableColors = page.elements.get('contributions').children.map(row => row.children[0].children[0].children[0].style.background);
    assert.deepEqual(tableColors, [color, color], 'accepted contributions and the chart share the same color');
  }
});

test('mint draft stays grey until a poll provides the wallet address’s assigned user ID', async () => {
  const page = await openPage('/mint');
  const first = page.snapshot.contributions[0];
  page.snapshot.contributions = [{ ...first, id: 5, user_id: 5, address: 'another-user' }];
  page.draws.length = 0;
  await page.refresh();
  assert.equal(page.elements.get('own-swatch').style.background, '#888');
  assert.equal(page.elements.get('draft-swatch').style.borderColor, '#888');
  assert.ok(page.draws.some(call => call.method === 'stroke' && call.strokeStyle === '#888'), 'an unregistered wallet does not guess its eventual user ID');
  assert.ok(page.draws.some(call => call.method === 'fill' && call.fillStyle === '#a3a3a3'));
  page.snapshot.contributions.push({ ...first, id: 6, user_id: 26 });
  page.draws.length = 0;
  await page.refresh();
  const color = userColor(26);
  assert.equal(page.elements.get('own-swatch').style.background, color);
  assert.equal(page.elements.get('draft-swatch').style.borderColor, color);
  assert.ok(page.draws.some(call => call.method === 'fill' && call.fillStyle === color));
  assert.ok(page.draws.some(call => call.method === 'stroke' && call.strokeStyle === color), 'the next snapshot switches to the server-assigned color');
});

test('user colors match across clients, reloads, reordered snapshots, and repeated contributions', async () => {
  const page = await openPage('/');
  const colorById = page => new Map(page.elements.get('contributions').children.map(row => [row.children[0].children[0].children[1].textContent.match(/#\d+$/)[0], row.children[0].children[0].children[0].style.background]));
  const first = page.snapshot.contributions[0];
  page.snapshot.contributions = Array.from({ length: 8 }, (_, index) => ({ ...first, id: 9 - index, user_id: index === 0 ? first.user_id : 9 - index, address: index === 0 ? first.address : `user-${9 - index}` })).concat(first);
  await page.refresh();
  const colors = colorById(page);
  assert.equal(colors.get('#1'), PALETTE[1]);
  assert.equal(colors.get('#2'), PALETTE[2]);
  assert.equal(colors.get('#6'), PALETTE[6]);
  assert.equal(colors.get('#7'), PALETTE[7]);
  assert.equal(colors.get('#8'), PALETTE[8]);
  assert.equal(colors.get('#9'), PALETTE[1], 'repeat user keeps their color regardless of contribution ID');
  page.snapshot.contributions.unshift({ ...first, id: 10, user_id: 26, address: 'next-user' });
  await page.refresh();
  assert.equal(colorById(page).get('#10'), PALETTE[2]);
  for (const [id, color] of colors) assert.equal(colorById(page).get(id), color, 'polls and additions do not recolor users');
  for (const storage of [page.storage, new Map(), new Map([['flame-mint-playground-palette-v1', JSON.stringify(PALETTE.slice().reverse())]])]) {
    const before = new Map(storage);
    const other = await openPage('/', storage);
    other.snapshot.contributions = page.snapshot.contributions.slice().reverse();
    await other.refresh();
    assert.deepEqual(colorById(other), colorById(page));
    assert.deepEqual(storage, before, 'palette storage is neither read nor rewritten');
  }
  assert.equal(page.storage.size, 0);
});

test('dashboard colors need no storage beyond independent unit, time and zoom preferences', async () => {
  const page = await openPage('/', { get(key) { assert.ok([UNIT_KEY, TIME_KEY, ZOOM_KEY].includes(key)); return null; }, set() { assert.fail('Unexpected storage write'); } });
  assert.equal(page.elements.get('connection').textContent, 'Live');
  const firstColor = page.elements.get('contributions').children[0].children[0].children[0].children[0].style.background;
  assert.equal(firstColor, userColor(1));
  await page.refresh();
  assert.equal(page.elements.get('contributions').children[0].children[0].children[0].children[0].style.background, firstColor);
});

test('the top mint rate follows the current block, ignoring historical totals and future plans', async () => {
  const page = await openPage('/');
  page.snapshot.contributions.push({ ...page.snapshot.contributions[0], id: 2, budget_sats: 200000, start_block: 11 });
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '40\u00a0sat/FLM', 'future contribution does not affect this block');
  page.snapshot.height = 11;
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '60\u00a0sat/FLM', '3,000 sats burned this block / 50 FLM');
  page.snapshot.height = 110;
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '20\u00a0sat/FLM', 'first contribution no longer burns');
  page.snapshot.height = 111;
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '—\u00a0sat/FLM', 'no issuance in an empty block');
});

test('budget slider values are satoshis directly, including its linear midpoint', async () => {
  const page = await openPage('/mint');
  for (const value of [BUDGET_MIN, 50000000, 50100000, BUDGET_MAX]) {
    page.elements.get('budget').value = String(value);
    await page.event('budget', 'input');
    assert.equal(page.elements.get('budget-value').value, new Intl.NumberFormat('en').format(value));
    assert.equal(page.elements.get('budget-unit').textContent, 'sat');
    assert.equal(page.elements.get('submit').disabled, value === BUDGET_MIN, 'unfunded fee combinations cannot be submitted');
  }
});

test('blocks and days are accessible persisted display modes independent from currency and raw contribution data', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const before = JSON.stringify(page.snapshot);
    assert.equal(page.elements.get('time-blocks')['aria-pressed'], 'true');
    assert.equal(page.elements.get('time-days')['aria-pressed'], 'false');
    assert.equal(page.storage.has(TIME_KEY), false);
    page.snapshot.height = 144 * 61 + 1;
    await page.refresh();
    page.draws.length = 0;
    await page.event('time-days', 'click');
    assert.equal(page.elements.get('time-days')['aria-pressed'], 'true');
    assert.equal(page.elements.get('time-blocks')['aria-pressed'], 'false');
    assert.equal(page.storage.get(TIME_KEY), 'days');
    assert.equal(page.elements.get('current-block').textContent, 'Mar 13');
    const ticks = page.draws.filter(call => call.canvas === page.elements.get('chart') && call.method === 'fillText' && call.args[2] === 267);
    assert.equal(ticks.length, 5);
    assert.equal(ticks[2].args[0], 'Mar 13', 'the center date represents the current block');
    assert.ok(ticks.every(call => /^[A-Z][a-z]{2} \d{1,2}$/.test(call.args[0])), 'all tick labels use short calendar dates');
    const dates = ticks.map(call => call.args[0]);
    await page.event('unit-btc', 'click');
    assert.equal(page.storage.get(TIME_KEY), 'days');
    assert.equal(page.storage.get(UNIT_KEY), 'BTC');
    assert.equal(page.elements.get('time-days')['aria-pressed'], 'true');
    const reloaded = await openPage(pathname, page.storage);
    assert.equal(reloaded.elements.get('time-days')['aria-pressed'], 'true');
    assert.equal(reloaded.elements.get('unit-btc')['aria-pressed'], 'true');
    assert.deepEqual(page.snapshot.contributions, JSON.parse(before).contributions, 'display changes never alter committed schedules');
    assert.equal(new Set(dates).size, 5);
  }
});

test('invalid or unavailable time preference storage falls back to blocks without breaking either switch', async () => {
  const storage = new Map([[TIME_KEY, 'invalid']]);
  const invalid = await openPage('/', storage);
  assert.equal(invalid.elements.get('time-blocks')['aria-pressed'], 'true');
  assert.equal(storage.get(TIME_KEY), 'invalid');
  const page = await openPage('/', { get() { throw new Error('Storage blocked'); }, set() { throw new Error('Storage blocked'); } });
  await page.event('time-days', 'click');
  assert.equal(page.elements.get('time-days')['aria-pressed'], 'true');
  assert.equal(page.elements.get('connection').textContent, 'Live');
  await page.event('unit-usd', 'click');
  assert.equal(page.elements.get('time-days')['aria-pressed'], 'true');
  assert.equal(page.elements.get('total-burned').textContent, '$50.000');
});

test('day-mode duration and delay show block-derived days while retaining every single-block slider value', async () => {
  const page = await openPage('/mint');
  await page.event('time-days', 'click');
  assert.match(page.elements.get('duration-min').textContent, /^1 day/);
  assert.match(page.elements.get('duration-max').textContent, /^31 day/);
  assert.match(page.elements.get('delay-min').textContent, /^0 day/);
  assert.match(page.elements.get('delay-max').textContent, /^31 day/);
  page.elements.get('duration').value = '145';
  await page.event('duration', 'input');
  page.elements.get('delay').value = '1';
  await page.event('delay', 'input');
  assert.equal(page.elements.get('duration-value').value, '1.007');
  assert.equal(page.elements.get('duration-unit').textContent, 'days');
  assert.equal(page.elements.get('delay-value').value, '0.007');
  assert.equal(page.elements.get('delay-unit').textContent, 'days');
  assert.equal(page.elements.get('duration').value, '145');
  assert.equal(page.elements.get('delay').value, '1');
  await page.event('contribution-form', 'submit');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(request.duration, 145);
  assert.equal(request.delay_blocks, 1);
  assert.equal(request.budget_sats, 1000000);
});

test('scheduled delayed drafts compete only across their actual start and expiry and survive retries exactly', async () => {
  const storage = new Map();
  const page = await openPage('/mint', storage, async () => { throw new Error('Response lost'); });
  page.elements.get('duration').value = '144';
  page.elements.get('delay').value = '4464';
  await page.event('duration', 'input');
  await page.event('delay', 'input');
  page.snapshot.contributions = [plan({ id: 1, address: 'other', start_block: 4475, duration: 144, budget_sats: 1000000 })];
  await page.refresh();
  assert.equal(page.elements.get('projected-reward').textContent, '3,600', 'all 144 delayed blocks overlap equal known competition');
  await page.event('contribution-form', 'submit');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(request.delay_blocks, 4464);
  assert.equal(request.duration, 144);
  assert.equal(page.elements.get('delay').disabled, true);
  await page.event('time-days', 'click');
  await page.event('unit-usd', 'click');
  const estimate = page.elements.get('projected-reward').textContent;
  await page.event('zoom-out', 'click');
  await page.event('zoom-in', 'click');
  assert.equal(page.elements.get('projected-reward').textContent, estimate);
  assert.deepEqual(JSON.parse(storage.get('flame-mint-playground-pending-v1')), request);
  const retry = await openPage('/mint', storage);
  assert.equal(Number(retry.elements.get('delay').value), 4464);
  assert.equal(Number(retry.elements.get('duration').value), 144);
  assert.equal(retry.elements.get('delay').disabled, true);
  await retry.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(retry.requests.find(request => request.url === '/api/contributions').options.body), request);
});

test('the dashed draft is limited to its real scheduled interval and disappears outside the chart', async () => {
  const page = await openPage('/mint');
  page.snapshot.height = 500;
  page.elements.get('duration').value = '144';
  await page.refresh();
  const strokePath = () => {
    let path = [];
    for (const call of page.draws.filter(call => call.canvas === page.elements.get('chart'))) {
      if (call.method === 'beginPath') path = [];
      if (call.method === 'moveTo' || call.method === 'lineTo') path.push(call.args);
      if (call.method === 'stroke' && call.strokeStyle === userColor(1)) return path;
    }
    return [];
  };
  for (const [delay, firstBlock, lastBlock] of [[0, 500, 645], [100, 600, 745], [DEFAULT_ZOOM - 1, 500 + DEFAULT_ZOOM - 1, 500 + DEFAULT_ZOOM]]) {
    page.draws.length = 0;
    page.elements.get('delay').value = String(delay);
    await page.event('delay', 'input');
    const path = strokePath();
    assert.ok(path.length > 0);
    const xs = path.map(point => point[0]);
    assert.equal(Math.min(...xs), 48 + (firstBlock - (500 - DEFAULT_ZOOM)) / (2 * DEFAULT_ZOOM) * 704, 'only one zero sample immediately precedes the actual start');
    assert.equal(Math.max(...xs), 48 + (lastBlock - (500 - DEFAULT_ZOOM)) / (2 * DEFAULT_ZOOM) * 704, 'only one zero sample immediately follows the actual end, clipped to the window');
    assert.equal(path.length, lastBlock - firstBlock + 1);
  }
  for (const delay of [DEFAULT_ZOOM, 4464]) {
    page.draws.length = 0;
    page.elements.get('delay').value = String(delay);
    await page.event('delay', 'input');
    assert.deepEqual(strokePath(), [], `delay ${delay} starts after the visible future window`);
    assert.notEqual(page.elements.get('projected-reward').textContent, '—', 'offscreen drafts still have full-duration estimates');
  }
});

test('legacy saved requests normalize absent delay to zero without rewriting their identity or historical duration', async () => {
  const pending = { address: 'tf1demo-address', request_id: 'legacy-request-001', budget_sats: 2000123, duration: 100 };
  const storage = new Map([['flame-mint-playground-pending-v1', JSON.stringify(pending)]]);
  const page = await openPage('/mint', storage);
  assert.equal(page.elements.get('submit').textContent, 'Retry pending');
  assert.equal(page.elements.get('duration-value').value, '100');
  assert.equal(page.elements.get('duration-unit').textContent, 'blocks');
  assert.equal(page.elements.get('delay').disabled, true);
  page.elements.get('duration').value = '144';
  page.elements.get('delay').value = '99';
  await page.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body), { ...pending, delay_blocks: 0 });
});

test('invalid saved delay or duration blocks new submissions without losing the saved request', async () => {
  const valid = { address: 'tf1demo-address', request_id: 'saved-request-0001', budget_sats: 10000000, duration: 144, delay_blocks: 0 };
  for (const change of [{ delay_blocks: -1 }, { delay_blocks: Number.MAX_SAFE_INTEGER + 1 }, { delay_blocks: 0.5 }, { delay_blocks: '1' }, { duration: 143 }, { duration: Number.MAX_SAFE_INTEGER + 1 }, { duration: 144.5 }]) {
    const saved = JSON.stringify({ ...valid, ...change });
    const storage = new Map([['flame-mint-playground-pending-v1', saved]]);
    const page = await openPage('/mint', storage);
    assert.equal(page.elements.get('submit').disabled, true);
    assert.equal(page.elements.get('delay').disabled, true);
    assert.match(page.elements.get('submit-message').textContent, /unreadable/);
    assert.equal(storage.get('flame-mint-playground-pending-v1'), saved);
  }
});

test('submitting locks budget, duration and delay until acceptance while display switches remain independent', async () => {
  let accept;
  const page = await openPage('/mint', new Map(), () => new Promise(resolve => { accept = resolve; }));
  page.elements.get('delay').value = '17';
  await page.event('delay', 'input');
  const submitting = page.event('contribution-form', 'submit');
  await page.settle();
  for (const id of ['budget', 'budget-value', 'duration', 'duration-value', 'delay', 'delay-value', 'submit', 'discard-pending']) assert.equal(page.elements.get(id).disabled, true, id);
  await page.event('time-days', 'click');
  await page.event('unit-btc', 'click');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(request.delay_blocks, 17);
  assert.equal(request.duration, 500);
  accept({ id: 2, user_id: 1, start_block: 28 });
  await submitting;
  for (const id of ['budget', 'budget-value', 'duration', 'duration-value', 'delay', 'delay-value', 'submit']) assert.equal(page.elements.get(id).disabled, false, id);
  assert.match(page.elements.get('submit-message').textContent, /accepted/);
});

test('mint page keeps keys local and persists the exact retry request across a page reload', async () => {
  const storage = new Map();
  const first = await openPage('/mint', storage, async () => { throw new Error('Response lost'); });
  assert.equal(first.elements.get('page-title').textContent, 'Mint');
  assert.equal(first.elements.get('nav-dashboard').textContent, 'Dashboard');
  assert.equal(first.elements.get('empty-table').textContent, 'No contributions.');
  assert.equal(first.elements.get('wallet-address').textContent, 'tf1demo-address');
  assert.equal(first.elements.get('wallet-status').textContent, 'Saved locally. Keys stay in this browser.');
  assert.equal(first.elements.get('submit').disabled, false);
  assert.equal(first.elements.get('submit').textContent, 'Submit');
  assert.equal(first.elements.get('seed-phrase').textContent, '');
  await first.event('contribution-form', 'submit');
  const original = JSON.parse(first.requests.find(request => request.url === '/api/contributions').options.body);
  assert.deepEqual(Object.keys(original).sort(), ['address', 'budget_sats', 'delay_blocks', 'duration', 'request_id']);
  assert.equal(original.delay_blocks, 0);
  assert.equal(first.elements.get('budget').disabled, true);
  assert.equal(first.elements.get('duration').disabled, true);
  assert.equal(first.elements.get('delay').disabled, true);
  assert.ok(storage.has('flame-mint-playground-pending-v1'));
  const reloaded = await openPage('/mint', storage);
  assert.equal(reloaded.elements.get('budget').disabled, true);
  assert.equal(reloaded.elements.get('submit').textContent, 'Retry pending');
  await reloaded.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(reloaded.requests.find(request => request.url === '/api/contributions').options.body), original);
  assert.equal(storage.has('flame-mint-playground-pending-v1'), false);
  assert.equal(reloaded.elements.get('budget').disabled, false);
  assert.equal(reloaded.elements.get('delay').disabled, false);
  assert.match(reloaded.elements.get('submit-message').textContent, /accepted/);
});

test('pending requests preserve exact non-step budgets after the slider migration', async () => {
  const pending = { address: 'tf1demo-address', request_id: 'old-request-00000001', budget_sats: 2000123, duration: 500 };
  const storage = new Map([['flame-mint-playground-pending-v1', JSON.stringify(pending)]]);
  const page = await openPage('/mint', storage);
  assert.equal(page.elements.get('budget').disabled, true);
  assert.equal(page.elements.get('budget-value').value, '2,000,123');
  assert.equal(page.elements.get('submit').textContent, 'Retry pending');
  // Native range inputs can normalize a non-step value; the saved request stays authoritative.
  page.elements.get('budget').value = '2000000';
  await page.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body), { ...pending, delay_blocks: 0 });
});

test('a corrupted saved wallet reports a recoverable error instead of silently replacing the identity', async () => {
  const storage = new Map([['flame-mint-playground-wallet-v1', '{broken json']]);
  const page = await openPage('/mint', storage);
  assert.match(page.elements.get('wallet-status').textContent, /Wallet unavailable/);
  assert.equal(page.elements.get('submit').disabled, true);
  assert.equal(page.elements.get('reset-wallet').hidden, false);
  assert.equal(storage.get('flame-mint-playground-wallet-v1'), '{broken json');
});

test('a saved reward address must match the address derived from its saved recovery phrase', async () => {
  const saved = JSON.stringify({ mnemonic: 'a browser only recovery phrase', address: 'tf1different-address' });
  const storage = new Map([['flame-mint-playground-wallet-v1', saved]]);
  const page = await openPage('/mint', storage);
  assert.match(page.elements.get('wallet-status').textContent, /does not match/);
  assert.equal(page.elements.get('submit').disabled, true);
  assert.equal(storage.get('flame-mint-playground-wallet-v1'), saved);
});

test('the moved wallet panel reveals its recovery phrase only on explicit disclosure and clears it on collapse', async () => {
  const page = await openPage('/mint');
  assert.equal(page.elements.get('wallet-panel').hidden, false);
  assert.equal(page.elements.get('seed-phrase').textContent, '');
  page.elements.get('seed-details').open = true;
  await page.event('seed-details', 'toggle');
  assert.equal(page.elements.get('seed-phrase').textContent, 'a browser only recovery phrase');
  page.elements.get('seed-details').open = false;
  await page.event('seed-details', 'toggle');
  assert.equal(page.elements.get('seed-phrase').textContent, '');
  assert.equal(page.elements.get('wallet-address').textContent, 'tf1demo-address');
  assert.equal(page.requests.filter(request => request.url === '/api/contributions').length, 0);
  assert.ok(page.requests.filter(request => request.url !== '/api/minter').every(request => request.options.body === undefined));
  assert.deepEqual(profileRequests(page).map(request => JSON.parse(request.options.body)), [{ address: 'tf1demo-address' }], 'registration sends only the public reward address, never the recovery phrase');
});

test('the moved wallet retains retry and confirmed reset behavior without silently replacing stored keys', async () => {
  const walletKey = 'flame-mint-playground-wallet-v1';
  const storage = new Map([[walletKey, '{broken json']]);
  let allowReset = false;
  const page = await openPage('/mint', storage, undefined, undefined, { confirm: () => allowReset });
  assert.equal(page.elements.get('wallet-panel').hidden, false);
  assert.equal(page.elements.get('retry-wallet').hidden, false);
  assert.equal(page.elements.get('reset-wallet').hidden, false);
  await page.event('retry-wallet', 'click');
  assert.equal(storage.get(walletKey), '{broken json', 'retry never destroys unreadable saved keys');
  await page.event('reset-wallet', 'click');
  assert.equal(storage.get(walletKey), '{broken json', 'cancelled reset preserves the saved wallet');
  assert.equal(page.elements.get('submit').disabled, true);
  allowReset = true;
  await page.event('reset-wallet', 'click');
  assert.equal(JSON.parse(storage.get(walletKey)).address, 'tf1demo-address');
  assert.equal(page.elements.get('retry-wallet').hidden, true);
  assert.equal(page.elements.get('reset-wallet').hidden, true);
  assert.equal(page.elements.get('submit').disabled, false);
  assert.equal(page.elements.get('seed-phrase').textContent, '');
  assert.equal(page.elements.get('wallet-panel').hidden, false);
});

test('money formatting preserves eight BTC decimals, truncates display values, and keeps small USD rates', () => {
  assert.equal(formatMoney(123456789), '123,456,789');
  assert.equal(formatMoney(123456789, 'BTC'), '1.23456789');
  assert.equal(formatMoney(1, 'BTC'), '0.00000001');
  assert.equal(formatMoney(0, 'BTC'), '0.00000000');
  assert.equal(formatMoney(100000000, 'BTC'), '1.00000000');
  assert.equal(formatMoney(1.99, 'BTC', null, true), '0.00000001');
  assert.equal(formatMoney(123456789, 'USD', 50000), '61,728.39');
  assert.equal(formatMoney(1999, 'USD', 50000), '0.99');
  assert.equal(formatMoney(1.99, 'USD', 50000, true), '0.000995', 'conversion uses the raw rate, not its truncated sat display');
  assert.equal(formatMoney(2000, 'USD', 50000, true), '1.00');
  assert.equal(formatMoney(1000, 'USD'), '—');
  assert.equal(formatMoney(1000, 'USD', NaN), '—');
  assert.equal(formatMoney(1000, 'USD', 0), '—');
});

test('the quote parser accepts only positive finite USD prices of BTC', () => {
  assert.equal(parseBtcUsdQuote({ data: { base: 'BTC', currency: 'USD', amount: '50000.12' } }), 50000.12);
  for (const amount of ['', ' ', '0', '-1', 'NaN', 'Infinity', '1e999', 50000, null]) {
    assert.throws(() => parseBtcUsdQuote({ data: { base: 'BTC', currency: 'USD', amount } }));
  }
  for (const payload of [null, {}, { data: {} }, { data: { base: 'ETH', currency: 'USD', amount: '50000' } }, { data: { base: 'BTC', currency: 'EUR', amount: '50000' } }]) {
    assert.throws(() => parseBtcUsdQuote(payload));
  }
});

const quoteRequests = page => page.requests.filter(request => request.url.startsWith('https:'));
const stateRequests = page => page.requests.filter(request => request.url.startsWith('/api/state'));
const rowValues = page => page.elements.get('contributions').children[0].children.slice(3).map(cell => cell.textContent);
const footerValues = page => page.elements.get('contribution-totals').children[0].children.slice(3).map(cell => cell.textContent);
const chartGeometry = page => page.draws.filter(call => call.canvas === page.elements.get('chart') && ['moveTo', 'lineTo'].includes(call.method)).map(call => [call.method, call.args[1]]);

test('both pages offer an accessible three-state unit selection while always showing the BTC/USD quote', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  const quoteRow = html.match(/<p\b[^>]*id="price-status"[^>]*>[^<]*<\/p>/)[0];
  assert.doesNotMatch(quoteRow, /\bhidden\b/);
  assert.match(quoteRow, />Loading BTC\/USD…<\/p>/);
  assert.match(html, /<div><p>Earned<\/p><p[^>]*id="total-reward">—&nbsp;FLM<\/p><\/div>/);
  assert.match(html, /<div><p>Mining fees<\/p><p[^>]*id="total-fees">—&nbsp;sat<\/p><\/div>/);
  assert.doesNotMatch(html.match(/id="(?:table-rate-label|projected-rate-label)"[^>]*>[^<]*/g).join(''), /incl\. fees/);
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    for (const [unit, selected] of [['sat', true], ['btc', false], ['usd', false]]) {
      assert.equal(page.elements.get(`unit-${unit}`)['aria-pressed'], String(selected));
    }
    assert.equal(quoteRequests(page).length, 1, 'the reference quote is fetched at startup in sat mode');
    assert.equal(page.elements.get('price-status').hidden, false);
    assert.match(page.elements.get('price-status').textContent, /BTC\/USD.*50,000|50,000.*BTC\/USD/);
    assert.equal(page.storage.has(UNIT_KEY), false, 'the default does not need a storage write');
    await page.event('unit-btc', 'click');
    assert.equal(page.elements.get('unit-btc')['aria-pressed'], 'true');
    assert.equal(page.elements.get('unit-sat')['aria-pressed'], 'false');
    assert.equal(quoteRequests(page).length, 1, 'unit changes reuse the loaded quote');
    assert.equal(stateRequests(page).length, 1, 'changing the unit reuses the same snapshot');
    assert.equal(page.elements.get('price-status').hidden, false);
    assert.equal(page.storage.get(UNIT_KEY), 'BTC');
  }
});

test('unit switches reuse the preloaded BTC/USD quote and preserve its refresh timer on both pages', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const row = page.elements.get('price-status').textContent;
    const quoteTimer = [...page.timers].find(([, timer]) => timer.delay === 60000)[0];
    for (const [unit, amount, rate] of [
      ['usd', pathname === '/' ? '$50.000' : '$1.000', '0.02\u00a0$/FLM'],
      ['btc', pathname === '/' ? '0.00100000\u00a0btc' : '0.00002000\u00a0btc', '0.00000040\u00a0BTC/FLM'],
      ['sat', pathname === '/' ? '100,000\u00a0sat' : '2,000\u00a0sat', '40\u00a0sat/FLM'],
    ]) {
      await page.event(`unit-${unit}`, 'click');
      assert.equal(page.elements.get('total-burned').textContent, amount);
      assert.equal(page.elements.get('total-fees').textContent, unit === 'usd' ? '$0.500' : unit === 'btc' ? '0.00001000\u00a0btc' : '1,000\u00a0sat');
      assert.equal(page.elements.get('total-reward').textContent, pathname === '/' ? '500\u00a0FLM' : '50\u00a0FLM');
      assert.equal(page.elements.get('total-rate').textContent, rate);
      assert.equal(page.elements.get('total-burned-label').textContent, 'Burned');
      assert.equal(page.elements.get('total-rate-label').textContent, pathname === '/' ? 'Current rate' : 'Effective rate');
      assert.equal(page.elements.get('price-status').hidden, false);
      assert.equal(page.elements.get('price-status').textContent, row, 'reference quote does not depend on the display unit');
      assert.equal(quoteRequests(page).length, 1, 'switching units never fetches a price');
      assert.equal([...page.timers].find(([, timer]) => timer.delay === 60000)[0], quoteTimer, 'switching units does not reset the refresh timer');
    }
  }
});

test('a pending quote refresh retains its previous value and USD display until success or failure', async () => {
  let calls = 0, finish;
  const page = await openPage('/', new Map(), undefined, undefined, { quote: () => ++calls === 1 ? quoteResponse(50000) : new Promise(resolve => { finish = resolve; }) });
  await page.event('unit-usd', 'click');
  const row = page.elements.get('price-status').textContent;
  const pending = page.timer(60000);
  await page.settle();
  assert.equal(page.elements.get('price-status').textContent, row, 'a refresh does not blink to Loading');
  assert.equal(page.elements.get('total-burned').textContent, '$50.000');
  await page.event('unit-sat', 'click');
  await page.event('unit-usd', 'click');
  assert.equal(quoteRequests(page).length, 2, 'unit changes do not duplicate the pending refresh');
  finish({ ok: false, status: 503 });
  await pending;
  assert.equal(page.elements.get('total-burned').textContent, '$—', 'failure clears the old quote');
  assert.equal(page.elements.get('price-status').hidden, false);
  assert.match(page.elements.get('price-status').textContent, /unavailable/i);
  assert.equal([...page.timers.values()].filter(timer => timer.delay === 60000).length, 1, 'failed refresh schedules exactly one retry');
});

test('BTC mode converts totals, tables, and rates on both pages without touching FLM rewards', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const snapshotBefore = JSON.stringify(page.snapshot);
    const rewardBefore = page.elements.get('total-reward').textContent;
    await page.event('unit-btc', 'click');
    assert.equal(page.elements.get('total-burned').textContent, `${pathname === '/' ? '0.00100000' : '0.00002000'}\u00a0btc`);
    assert.equal(page.elements.get('total-fees').textContent, '0.00001000\u00a0btc');
    assert.equal(page.elements.get('total-rate').textContent, '0.00000040\u00a0BTC/FLM');
    assert.deepEqual(rowValues(page), ['0.00002000', '50', '0.00001000', '0.00000040']);
    assert.deepEqual(footerValues(page), pathname === '/' ? ['0.00100000', '500', '0.00001000', '0.00000200'] : ['0.00002000', '50', '0.00001000', '0.00000040']);
    assert.equal(page.elements.get('total-reward').textContent, rewardBefore);
    assert.equal(page.elements.get('total-burned-label').textContent, 'Burned');
    assert.equal(page.elements.get('total-rate-label').textContent, pathname === '/' ? 'Current rate' : 'Effective rate');
    assert.match(page.elements.get('table-burned-label').textContent, /BTC/);
    assert.match(page.elements.get('table-fees-label').textContent, /BTC/);
    assert.match(page.elements.get('table-rate-label').textContent, /BTC\s*\/\s*FLM/);
    assert.equal(page.elements.get('chart-burn-unit').textContent, 'BTC/block');
    assert.equal(page.elements.get('chart-rate-unit').textContent, 'BTC/FLM');
    assert.match(page.elements.get('chart')['aria-label'], /BTC/);
    assert.equal(JSON.stringify(page.snapshot), snapshotBefore);
    assert.equal(stateRequests(page).length, 1);
  }
});

test('mint budget, fees, contribution labels, validation, and projection follow the unit without altering the draft', async () => {
  const page = await openPage('/mint');
  const budgetBefore = page.elements.get('budget').value;
  const projectedBefore = page.elements.get('projected-reward').textContent;
  await page.event('unit-btc', 'click');
  assert.equal(page.elements.get('budget-value').value, '0.01000000');
  assert.equal(page.elements.get('budget-unit').textContent, 'BTC');
  assert.equal(page.elements.get('budget')['aria-valuetext'], '0.01000000 BTC');
  assert.equal(page.elements.get('budget-min').textContent, '0.00100000 BTC');
  assert.equal(page.elements.get('budget-max').textContent, '1.00000000 BTC');
  assert.equal(page.elements.get('projected-fees').textContent, '0.00500000 BTC');
  assert.equal(page.elements.get('projected-burn').textContent, '0.00500000 BTC');
  assert.match(page.elements.get('fee-label').textContent, /0\.00001000 BTC/);
  assert.match(page.elements.get('contributions').children[0].children[0].children[1].textContent, /0\.00300000 BTC/);
  const draft = { budget_sats: 1000000, duration: 500, start_block: 11 };
  const estimate = projectedReward(page.snapshot.contributions, draft, 'tf1demo-address', 10);
  assert.equal(page.elements.get('projected-rate').textContent, `${formatMoney((draft.budget_sats - draft.duration * FEE) * SPARKS / estimate, 'BTC', null, true)} BTC / FLM`);
  assert.equal(page.elements.get('projected-reward').textContent, projectedBefore);
  assert.equal(page.elements.get('budget').value, budgetBefore);
  page.elements.get('budget').value = String(BUDGET_MIN);
  await page.event('budget', 'input');
  assert.equal(page.elements.get('submit').disabled, true);
  assert.match(page.elements.get('budget-error').textContent, /0\.00500500 BTC/);
  assert.doesNotMatch(page.elements.get('budget-error').textContent, /\bsat\b/);
});

test('USD mode reuses the public quote fetched without identity and converts all accounting displays, not accounting', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const snapshotBefore = JSON.stringify(page.snapshot);
    await page.event('unit-usd', 'click');
    const requests = quoteRequests(page);
    assert.equal(requests.length, 1);
    const request = requests[0];
    assert.equal(request.url, 'https://api.coinbase.com/v2/prices/BTC-USD/spot');
    assert.equal(request.options.credentials, 'omit');
    assert.equal(request.options.referrerPolicy, 'no-referrer');
    assert.equal(request.options.body, undefined, 'wallet addresses and keys are never sent to the quote API');
    assert.ok(request.options.signal instanceof AbortSignal);
    assert.equal(page.abortTimeouts.length, pathname === '/mint' ? 3 : 2, 'the snapshot, quote and mint profile each have a timeout');
    assert.ok(page.abortTimeouts.every(delay => delay === 10000), 'price requests cannot hang beyond ten seconds');
    assert.equal(page.elements.get('price-status').hidden, false);
    assert.match(page.elements.get('price-status').textContent, /50,000|50000/);
    assert.equal(page.elements.get('total-burned').textContent, pathname === '/' ? '$50.000' : '$1.000');
    assert.equal(page.elements.get('total-fees').textContent, '$0.500');
    assert.equal(page.elements.get('total-rate').textContent, '0.02\u00a0$/FLM');
    assert.deepEqual(rowValues(page), ['1.00', '50', '0.50', '0.02']);
    assert.deepEqual(footerValues(page), pathname === '/' ? ['50.00', '500', '0.50', '0.10'] : ['1.00', '50', '0.50', '0.02']);
    assert.equal(page.elements.get('chart-burn-unit').textContent, '$/block');
    assert.equal(page.elements.get('chart-rate-unit').textContent, '$/FLM');
    assert.equal(page.elements.get('total-rate-label').textContent, pathname === '/' ? 'Current rate' : 'Effective rate');
    if (pathname === '/mint') {
      assert.equal(page.elements.get('budget-value').value, '500.00');
      assert.equal(page.elements.get('budget-unit').textContent, 'USD');
      assert.equal(page.elements.get('projected-fees').textContent, '$250.00');
      assert.equal(page.elements.get('projected-burn').textContent, '$250.00');
      assert.equal(page.elements.get('budget-min').textContent, '$50.00');
      assert.equal(page.elements.get('budget-max').textContent, '$50,000.00');
      assert.match(page.elements.get('projected-rate').textContent, /^\$[\d,.]+ \/ FLM$/);
    }
    assert.equal(JSON.stringify(page.snapshot), snapshotBefore);
    assert.equal(stateRequests(page).length, 1);
    assert.equal([...page.storage.values()].some(value => value === '50000'), false, 'quotes are not persisted');
  }
});

test('USD rates convert raw fractional sat/FLM rather than the truncated sat display', async () => {
  const page = await openPage('/mint');
  const contribution = page.snapshot.contributions[0];
  contribution.burned_sats = '199'; contribution.fees_sats = '0'; contribution.reward_sparks = '10000000000';
  await page.refresh();
  assert.equal(page.elements.get('total-rate').textContent, '1\u00a0sat/FLM');
  await page.event('unit-usd', 'click');
  assert.equal(page.elements.get('total-rate').textContent, '0.000995\u00a0$/FLM');
  assert.equal(rowValues(page)[3], '0.000995');
  assert.equal(footerValues(page)[3], '0.000995');
  assert.equal(page.elements.get('total-reward').textContent, '100\u00a0FLM');
});

test('top counters on both pages keep units without label prefixes and retain three truncated USD decimals across updates', async () => {
  for (const pathname of ['/', '/mint']) {
    let price = 50000;
    const page = await openPage(pathname, new Map(), undefined, undefined, { quote: () => quoteResponse(price) });
    page.snapshot.totals.burned_sats = '1999';
    page.snapshot.contributions[0].burned_sats = '1999';
    await page.refresh();
    const before = JSON.stringify(page.snapshot);
    assert.equal(page.elements.get('total-burned').textContent, '1,999\u00a0sat');
    await page.event('unit-btc', 'click');
    assert.equal(page.elements.get('total-burned').textContent, '0.00001999\u00a0btc');
    await page.event('unit-usd', 'click');
    assert.equal(page.elements.get('total-burned').textContent, '$0.999', 'top USD amounts truncate, rather than round, to exactly three places');
    assert.equal(rowValues(page)[0], '0.99', 'table precision is unchanged');
    assert.equal(page.elements.get('total-reward').textContent, `${pathname === '/' ? '500' : '50'}\u00a0FLM`);
    assert.match(page.elements.get('total-rate').textContent, /^[\d,.]+\u00a0\$\/FLM$/);
    assert.equal(JSON.stringify(page.snapshot), before, 'labels do not alter accounting');
    page.snapshot.totals.burned_sats = '2000';
    page.snapshot.contributions[0].burned_sats = '2000';
    await page.refresh();
    assert.equal(page.elements.get('total-burned').textContent, '$1.000');
    price = 100000;
    await page.timer(60000);
    assert.equal(page.elements.get('total-burned').textContent, '$2.000', 'quote refresh keeps the self-contained format');
    await page.event('unit-sat', 'click');
    assert.equal(page.elements.get('total-burned').textContent, '2,000\u00a0sat');
  }
});

test('unknown wallet totals and undefined rates keep their labels and selected units', async () => {
  const storage = new Map([['flame-mint-playground-wallet-v1', '{broken json']]);
  const page = await openPage('/mint', storage);
  for (const [unit, amount, rateUnit] of [['sat', '—\u00a0sat', 'sat'], ['btc', '—\u00a0btc', 'BTC'], ['usd', '$—', '$']]) {
    await page.event(`unit-${unit}`, 'click');
    await page.refresh();
    assert.equal(page.elements.get('total-burned').textContent, amount);
    assert.equal(page.elements.get('total-fees').textContent, amount);
    assert.equal(page.elements.get('total-reward').textContent, '—\u00a0FLM');
    assert.equal(page.elements.get('total-rate').textContent, `—\u00a0${rateUnit}/FLM`);
  }
  for (const pathname of ['/', '/mint']) {
    const ready = await openPage(pathname);
    ready.snapshot.contributions = [];
    ready.snapshot.totals = { contribution_count: 0, burned_sats: '0', fees_sats: '0', reward_sparks: '0' };
    await ready.refresh();
    for (const [unit, amount, rateUnit] of [['sat', '0\u00a0sat', 'sat'], ['btc', '0.00000000\u00a0btc', 'BTC'], ['usd', '$0.000', '$']]) {
      await ready.event(`unit-${unit}`, 'click');
      assert.equal(ready.elements.get('total-burned').textContent, amount);
      assert.equal(ready.elements.get('total-fees').textContent, amount);
      assert.equal(ready.elements.get('total-reward').textContent, '0\u00a0FLM');
      assert.equal(ready.elements.get('total-rate').textContent, `—\u00a0${rateUnit}/FLM`);
    }
  }
});

test('both chart axes use selected currency while retaining the raw burn and fee scale', async () => {
  for (const pathname of ['/', '/mint']) {
    const page = await openPage(pathname);
    const ticks = () => page.draws.filter(call => call.canvas === page.elements.get('chart') && call.method === 'fillText' && call.args[2] !== 267);
    const parseAxis = text => Number(String(text).replaceAll(',', '').replace(/k$/, 'e3').replace(/m$/, 'e6'));
    const satTicks = ticks().filter(call => call.args[1] < 50).map(call => [parseAxis(call.args[0]), call.args[2]]);
    assert.ok(satTicks.some(([sats]) => sats > 0));
    if (pathname === '/') assert.ok(satTicks.some(([sats]) => sats < 0), 'the dashboard fee region is covered');
    for (const unit of ['BTC', 'USD']) {
      page.draws.length = 0;
      await page.event(`unit-${unit.toLowerCase()}`, 'click');
      const converted = ticks();
      for (const [sats, y] of satTicks) {
        const pair = converted.filter(call => call.args[2] === y);
        assert.equal(pair.length, 2, 'a shared horizontal line still has both vertical labels');
        assert.equal(pair[0].args[0], formatMoney(sats, unit, 50000));
        assert.equal(pair[1].args[0], formatMoney(sats / 50, unit, 50000, true));
      }
    }
  }
});

test('USD pending and failed quotes show unavailable amounts, never fictitious zeroes, while the snapshot stays live', async () => {
  let finish;
  const page = await openPage('/mint', new Map(), undefined, undefined, { quote: () => new Promise(resolve => { finish = resolve; }) });
  await page.event('unit-usd', 'click');
  assert.match(page.elements.get('price-status').textContent, /Loading/i);
  assert.equal(page.elements.get('total-burned').textContent, '$—');
  assert.equal(page.elements.get('total-fees').textContent, '$—');
  assert.equal(page.elements.get('total-rate').textContent, '—\u00a0$/FLM');
  assert.equal(page.elements.get('budget-value').value, '—');
  for (const id of ['projected-fees', 'projected-burn', 'projected-rate']) assert.equal(page.elements.get(id).textContent, '—', id);
  assert.deepEqual(rowValues(page), ['—', '50', '—', '—']);
  assert.equal(page.elements.get('total-reward').textContent, '50\u00a0FLM');
  finish({ ok: false, status: 503 });
  await page.settle();
  assert.match(page.elements.get('price-status').textContent, /unavailable|503/i);
  assert.equal(page.elements.get('total-burned').textContent, '$—');
  assert.equal(page.elements.get('total-fees').textContent, '$—');
  assert.equal(page.elements.get('connection').textContent, 'Live');
  await page.event('unit-btc', 'click');
  assert.equal(page.elements.get('total-burned').textContent, '0.00002000\u00a0btc');
  assert.equal(page.elements.get('price-status').hidden, false);
  assert.match(page.elements.get('price-status').textContent, /unavailable/i);
});

test('invalid, failed, and unreadable quotes leave USD unknown and retain functioning offline units', async () => {
  const variants = [
    () => { throw new Error('Network unavailable'); },
    () => ({ ok: true, json: async () => { throw new Error('Invalid JSON'); } }),
    ...['0', '-1', 'NaN'].map(amount => () => quoteResponse(amount)),
    () => ({ ok: true, json: async () => ({ data: { base: 'ETH', currency: 'USD', amount: '50000' } }) }),
  ];
  for (const quote of variants) {
    const page = await openPage('/', new Map(), undefined, undefined, { quote });
    await page.event('unit-usd', 'click');
    assert.equal(page.elements.get('total-burned').textContent, '$—');
    assert.match(page.elements.get('price-status').textContent, /unavailable/i);
    await page.event('unit-sat', 'click');
    assert.equal(page.elements.get('total-burned').textContent, '100,000\u00a0sat');
    assert.equal(page.elements.get('connection').textContent, 'Live');
  }
});

test('BTC/USD refreshes every minute in all units and discards stale values on failure', async () => {
  let count = 0;
  const page = await openPage('/', new Map(), undefined, undefined, { quote: () => ++count === 3 ? { ok: false, status: 503 } : quoteResponse(count * 50000) });
  await page.event('unit-usd', 'click');
  assert.equal(page.elements.get('total-burned').textContent, '$50.000');
  await page.timer(60000);
  assert.equal(page.elements.get('total-burned').textContent, '$100.000');
  assert.equal(quoteRequests(page).length, 2);
  await page.timer(60000);
  assert.equal(page.elements.get('total-burned').textContent, '$—', 'a failed refresh does not keep an apparently current old price');
  assert.match(page.elements.get('price-status').textContent, /unavailable/i);
  await page.event('unit-btc', 'click');
  assert.equal([...page.timers.values()].filter(timer => timer.delay === 60000).length, 1);
  assert.equal(page.elements.get('price-status').hidden, false);
  assert.equal(quoteRequests(page).length, 3, 'leaving USD does not fetch again');
  await page.timer(60000);
  assert.equal(quoteRequests(page).length, 4, 'the quote refreshes while BTC is selected');
  assert.equal(page.elements.get('total-burned').textContent, '0.00100000\u00a0btc');
  assert.match(page.elements.get('price-status').textContent, /200,000/);
  await page.event('unit-sat', 'click');
  await page.timer(60000);
  assert.equal(quoteRequests(page).length, 5, 'the quote refreshes while sat is selected');
  assert.equal(page.elements.get('total-burned').textContent, '100,000\u00a0sat');
  assert.equal(stateRequests(page).length, 1, 'quote refresh never fetches contribution history');
});

test('rapid unit changes reuse the in-flight quote and do not overlap or replace the selected unit', async () => {
  let finish;
  const page = await openPage('/', new Map(), undefined, undefined, { quote: () => new Promise(resolve => { finish = resolve; }) });
  await page.event('unit-usd', 'click');
  await page.event('unit-btc', 'click');
  await page.event('unit-usd', 'click');
  assert.equal(quoteRequests(page).length, 1);
  await page.event('unit-sat', 'click');
  finish(quoteResponse(50000));
  await page.settle();
  assert.equal(page.elements.get('unit-sat')['aria-pressed'], 'true');
  assert.equal(page.elements.get('total-burned').textContent, '100,000\u00a0sat');
  assert.equal(page.elements.get('price-status').hidden, false);
  assert.match(page.elements.get('price-status').textContent, /50,000/);
  assert.equal([...page.timers.values()].filter(timer => timer.delay === 60000).length, 1);
});

test('unit preferences persist, invalid values fall back to sats, and blocked storage never blocks the dashboard', async () => {
  for (const unit of ['sat', 'BTC', 'USD', 'invalid']) {
    const storage = new Map([[UNIT_KEY, unit]]);
    const page = await openPage('/', storage);
    assert.equal(page.elements.get(`unit-${unit === 'invalid' ? 'sat' : unit.toLowerCase()}`)['aria-pressed'], 'true');
    assert.equal(quoteRequests(page).length, 1, 'all initial units fetch the reference quote');
    assert.equal(storage.get(UNIT_KEY), unit, 'startup does not rewrite stored preferences or store quotes');
  }
  const page = await openPage('/', { get() { throw new Error('Storage blocked'); }, set() { throw new Error('Storage blocked'); } });
  assert.equal(page.elements.get('unit-sat')['aria-pressed'], 'true');
  await page.event('unit-btc', 'click');
  assert.equal(page.elements.get('total-burned').textContent, '0.00100000\u00a0btc');
  assert.equal(page.elements.get('connection').textContent, 'Live');
});

test('unit changes leave chart data, forecast, requests, and pending idempotence in satoshis', async () => {
  const storage = new Map();
  const page = await openPage('/mint', storage, async () => { throw new Error('Response lost'); });
  const projectedBefore = page.elements.get('projected-reward').textContent;
  const capture = async unit => {
    page.draws.length = 0;
    await page.event(`unit-${unit}`, 'click');
    return chartGeometry(page);
  };
  const satGeometry = await capture('sat');
  assert.deepEqual(await capture('btc'), satGeometry, 'chart heights retain raw-satoshi accounting');
  assert.deepEqual(await capture('usd'), satGeometry);
  assert.equal(page.elements.get('projected-reward').textContent, projectedBefore);
  assert.equal(page.elements.get('budget').value, '1000000');
  await page.event('contribution-form', 'submit');
  const request = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(request.budget_sats, 1000000);
  assert.equal(request.duration, 500);
  assert.deepEqual(Object.keys(request).sort(), ['address', 'budget_sats', 'delay_blocks', 'duration', 'request_id']);
  assert.deepEqual(JSON.parse(storage.get('flame-mint-playground-pending-v1')), request);
  const reloaded = await openPage('/mint', storage);
  assert.equal(reloaded.elements.get('unit-usd')['aria-pressed'], 'true');
  await reloaded.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(reloaded.requests.find(request => request.url === '/api/contributions').options.body), request);
  assert.equal(storage.has('flame-mint-playground-pending-v1'), false);
});

test('typed-value helpers parse native units while slider presentation snaps to its own limits', () => {
  assert.equal(parseInputNumber('1,000,000 sat'), 1000000);
  assert.equal(parseInputNumber('$3,665.73'), 3665.73);
  assert.equal(parseInputNumber('0.01000000 BTC'), 0.01);
  assert.equal(parseInputNumber('1.007 days'), 1.007);
  assert.equal(parseInputNumber(''), null);
  assert.equal(parseInputNumber('abc'), null);
  assert.equal(parseInputNumber('—'), null);
  assert.equal(snapToRange(123456, BUDGET_MIN, BUDGET_MAX, BUDGET_STEP), 100000);
  assert.equal(snapToRange(150000, BUDGET_MIN, BUDGET_MAX, BUDGET_STEP), 200000);
  assert.equal(snapToRange(1, BUDGET_MIN, BUDGET_MAX, BUDGET_STEP), BUDGET_MIN);
  assert.equal(snapToRange(1e12, BUDGET_MIN, BUDGET_MAX, BUDGET_STEP), BUDGET_MAX);
  assert.equal(snapToRange(10, DURATION_MIN, DURATION_MAX), DURATION_MIN);
  assert.equal(snapToRange(999999, DURATION_MIN, DURATION_MAX), DURATION_MAX);
  assert.equal(snapToRange(200.4, DURATION_MIN, DURATION_MAX), 200);
  assert.equal(snapToRange(null, 0, DELAY_MAX), null);
  assert.equal(inputToSats(5, 'sat'), 5);
  assert.equal(inputToSats(0.01, 'BTC'), 1000000);
  assert.equal(inputToSats(500, 'USD', 50000), 1000000);
  assert.equal(inputToSats(500, 'USD', null), null, 'USD needs a quote');
  assert.equal(inputToBlocks(2, 'days'), 288);
  assert.equal(inputToBlocks(300, 'blocks'), 300);
});

test('a typed budget stays exact beyond the slider maximum while the slider remains bounded', async () => {
  const page = await openPage('/mint');
  const typed = page.elements.get('budget-value');
  typed.value = '2,550,000';
  await page.event('budget-value', 'change');
  assert.equal(page.elements.get('budget').value, '2600000', 'slider snapped to the 100,000 sat step');
  assert.equal(typed.value, '2,550,000', 'field keeps the exact amount');
  assert.equal(page.elements.get('budget-unit').textContent, 'sat', 'unit sits outside the box');
  assert.equal(page.elements.get('projected-fees').textContent, '500,000 sat');
  assert.equal(page.elements.get('projected-burn').textContent, '2,050,000 sat', 'estimates use the exact amount');
  await page.event('contribution-form', 'submit');
  const sent = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(sent.budget_sats, 2550000, 'the exact amount is submitted');
  const fresh = await openPage('/mint');
  const field = fresh.elements.get('budget-value');
  field.value = '999999999999';
  await fresh.event('budget-value', 'change');
  assert.equal(fresh.elements.get('budget').value, String(BUDGET_MAX), 'only the slider is clamped to its maximum');
  assert.equal(field.value, '999,999,999,999');
  assert.equal(fresh.elements.get('projected-burn').textContent, '999,999,499,999 sat');
  field.value = '9999999999999999999999999';
  await fresh.event('budget-value', 'change');
  assert.equal(field.value, new Intl.NumberFormat('en').format(Number.MAX_SAFE_INTEGER), 'the precision ceiling is independent of the slider maximum');
  field.value = '5';
  await fresh.event('budget-value', 'change');
  assert.equal(fresh.elements.get('budget').value, String(BUDGET_MIN), 'clamped to the minimum');
  assert.equal(field.value, '100,000');
  field.value = '123,456';
  await fresh.event('budget-value', 'change');
  assert.equal(field.value, '123,456');
  fresh.elements.get('budget').value = '300000';
  await fresh.event('budget', 'input');
  assert.equal(field.value, '300,000', 'dragging the slider replaces the typed amount');
  field.value = 'garbage';
  await fresh.event('budget-value', 'change');
  assert.equal(fresh.elements.get('budget').value, '300000', 'unparseable input leaves the slider alone');
  assert.equal(field.value, '300,000', 'field is re-rendered from the slider');
});

test('typing a budget respects the selected currency', async () => {
  const page = await openPage('/mint', new Map([[UNIT_KEY, 'USD']]));
  const typed = page.elements.get('budget-value');
  typed.value = '$500';
  await page.event('budget-value', 'change');
  assert.equal(page.elements.get('budget').value, '1000000', '$500 at $50,000/BTC is 1,000,000 sat');
  assert.equal(typed.value, '500.00');
  assert.equal(page.elements.get('budget-unit').textContent, 'USD');
  await page.event('unit-btc', 'click');
  typed.value = '0.025';
  await page.event('budget-value', 'change');
  assert.equal(page.elements.get('budget').value, '2500000');
  assert.equal(typed.value, '0.02500000');
  assert.equal(page.elements.get('budget-unit').textContent, 'BTC');
});

test('typed durations and delays convert days to blocks beyond the slider bounds and retain their minimums', async () => {
  const page = await openPage('/mint', new Map([[TIME_KEY, 'days']]));
  const duration = page.elements.get('duration-value');
  duration.value = '2';
  await page.event('duration-value', 'change');
  assert.equal(page.elements.get('duration').value, '288', '2 days is 288 blocks');
  assert.equal(duration.value, '2');
  assert.equal(page.elements.get('duration-unit').textContent, 'days');
  duration.value = '100';
  await page.event('duration-value', 'change');
  assert.equal(page.elements.get('duration').value, String(DURATION_MAX));
  assert.equal(duration.value, '100', 'a typed duration is not limited to the 31-day slider');
  await page.event('time-blocks', 'click');
  assert.equal(duration.value, '14,400');
  await page.event('time-days', 'click');
  duration.value = '0';
  await page.event('duration-value', 'change');
  assert.equal(page.elements.get('duration').value, String(DURATION_MIN));
  await page.event('time-blocks', 'click');
  const delay = page.elements.get('delay-value');
  delay.value = '10.6';
  await page.event('delay-value', 'change');
  assert.equal(page.elements.get('delay').value, '11', 'blocks are whole numbers');
  delay.value = '-5';
  await page.event('delay-value', 'change');
  assert.equal(page.elements.get('delay').value, '5', 'the sign is ignored and the value stays within bounds');
  delay.value = '10000';
  await page.event('delay-value', 'change');
  assert.equal(page.elements.get('delay').value, String(DELAY_MAX));
  assert.equal(delay.value, '10,000', 'delays can exceed the visible slider range');
});

test('larger typed values survive polls and display changes, and each slider replaces only its own field', async () => {
  const page = await openPage('/mint');
  for (const [id, value] of [['budget', '1000000000123'], ['duration', '12345'], ['delay', '67890']]) {
    page.elements.get(`${id}-value`).value = value;
    await page.event(`${id}-value`, 'change');
  }
  const check = () => {
    assert.equal(page.elements.get('budget-value').value, '1,000,000,000,123');
    assert.equal(page.elements.get('duration-value').value, '12,345');
    assert.equal(page.elements.get('delay-value').value, '67,890');
    assert.equal(page.elements.get('budget').value, String(BUDGET_MAX));
    assert.equal(page.elements.get('duration').value, String(DURATION_MAX));
    assert.equal(page.elements.get('delay').value, String(DELAY_MAX));
  };
  check();
  await page.refresh();
  await page.event('unit-btc', 'click');
  assert.equal(page.elements.get('budget-value').value, '10,000.00000123');
  await page.event('unit-sat', 'click');
  await page.event('time-days', 'click');
  await page.event('time-blocks', 'click');
  await page.event('zoom-out', 'click');
  await page.event('zoom-in', 'click');
  check();
  page.elements.get('duration').value = '500';
  await page.event('duration', 'input');
  assert.equal(page.elements.get('duration-value').value, '500');
  assert.equal(page.elements.get('budget-value').value, '1,000,000,000,123');
  assert.equal(page.elements.get('delay-value').value, '67,890');
  page.elements.get('budget').value = '2000000';
  await page.event('budget', 'input');
  assert.equal(page.elements.get('budget-value').value, '2,000,000');
  assert.equal(page.elements.get('delay-value').value, '67,890');
  page.elements.get('delay').value = '7';
  await page.event('delay', 'input');
  assert.equal(page.elements.get('delay-value').value, '7');
  assert.equal(page.elements.get('duration-value').value, '500');
});

test('large manual requests persist exact values and retry unchanged despite clamped locked sliders', async () => {
  const storage = new Map();
  const page = await openPage('/mint', storage, async () => { throw new Error('Response lost'); });
  const values = { budget: 1000000000123, duration: 12345, delay: 67890 };
  for (const [id, value] of Object.entries(values)) {
    page.elements.get(`${id}-value`).value = String(value);
    await page.event(`${id}-value`, 'change');
  }
  await page.event('contribution-form', 'submit');
  const sent = JSON.parse(page.requests.find(request => request.url === '/api/contributions').options.body);
  assert.equal(sent.budget_sats, values.budget);
  assert.equal(sent.duration, values.duration);
  assert.equal(sent.delay_blocks, values.delay);
  const retry = await openPage('/mint', storage);
  for (const [id, value] of Object.entries(values)) {
    assert.equal(retry.elements.get(id).disabled, true);
    assert.equal(retry.elements.get(`${id}-value`).disabled, true);
    assert.equal(retry.elements.get(`${id}-value`).value, new Intl.NumberFormat('en').format(value));
    retry.elements.get(id).value = '1';
  }
  await retry.event('time-days', 'click');
  await retry.event('unit-btc', 'click');
  await retry.event('contribution-form', 'submit');
  assert.deepEqual(JSON.parse(retry.requests.find(request => request.url === '/api/contributions').options.body), sent);
  await retry.event('unit-sat', 'click');
  await retry.event('time-blocks', 'click');
  for (const [id, value] of Object.entries(values)) {
    assert.equal(retry.elements.get(`${id}-value`).disabled, false);
    assert.equal(retry.elements.get(`${id}-value`).value, new Intl.NumberFormat('en').format(value), 'successful retry preserves the full accepted amount for the next draft');
  }
});

test('manual schedules must fund every block and keep their exclusive end within safe integer precision', async () => {
  const page = await openPage('/mint');
  page.elements.get('duration-value').value = '10000';
  await page.event('duration-value', 'change');
  assert.equal(page.elements.get('submit').disabled, true, 'manual durations still need enough funds for burn and mining fees');
  page.elements.get('budget-value').value = '10010000';
  await page.event('budget-value', 'change');
  assert.equal(page.elements.get('submit').disabled, false);
  page.elements.get('duration-value').value = '144';
  await page.event('duration-value', 'change');
  const safeDelay = Number.MAX_SAFE_INTEGER - page.snapshot.height - 1 - 144;
  page.elements.get('delay-value').value = String(safeDelay);
  await page.event('delay-value', 'change');
  assert.equal(page.elements.get('submit').disabled, false, 'an exclusive end exactly at MAX_SAFE_INTEGER is valid');
  page.elements.get('delay-value').value = String(safeDelay + 1);
  await page.event('delay-value', 'change');
  assert.equal(page.elements.get('submit').disabled, true, 'an overflowing exclusive end cannot be submitted');
  await page.event('contribution-form', 'submit');
  assert.equal(page.requests.some(request => request.url === '/api/contributions'), false);
});

test('Enter in a typed field applies the value without submitting the form', async () => {
  const page = await openPage('/mint');
  const typed = page.elements.get('budget-value');
  typed.value = '3,000,000';
  let prevented = false;
  await typed.handlers.keydown({ key: 'Enter', preventDefault() { prevented = true; } });
  await page.settle();
  assert.equal(prevented, true);
  assert.equal(page.elements.get('budget').value, '3000000');
  assert.equal(page.requests.some(request => request.url === '/api/contributions'), false, 'no contribution was submitted');
  await typed.handlers.keydown({ key: 'a', preventDefault() { prevented = 'again'; } });
  assert.equal(prevented, true, 'other keys are not intercepted');
});

test('typed fields are locked together with the sliders while a request is pending', async () => {
  const pending = { address: 'tf1demo-address', request_id: 'old-request-00000002', budget_sats: 2000000, duration: 500 };
  const page = await openPage('/mint', new Map([['flame-mint-playground-pending-v1', JSON.stringify(pending)]]));
  for (const id of ['budget', 'duration', 'delay']) {
    assert.equal(page.elements.get(id).disabled, true, id);
    assert.equal(page.elements.get(`${id}-value`).disabled, true, `${id}-value`);
  }
  const typed = page.elements.get('budget-value');
  typed.value = '9,000,000';
  await page.event('budget-value', 'change');
  assert.equal(Number(page.elements.get('budget').value), 2000000, 'a locked slider does not move');
  assert.equal(typed.value, '2,000,000');
});
