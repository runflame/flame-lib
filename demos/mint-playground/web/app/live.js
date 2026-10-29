// Live engine state for the main app: server snapshots, the browser wallet, the
// minter profile and contribution submissions. The design's component reads
// `window.FlameLive` and re-renders on every `flamelive` event.
import { BUDGET_MIN, DURATION_MIN, contributionFee, parseBtcUsdQuote, projectedReward } from '/app.js';

// Shared with the classic pages, so both use one identity and one pending request.
const WALLET_KEY = 'flame-mint-playground-wallet-v1';
const PENDING_KEY = 'flame-mint-playground-pending-v1';
// Wallets left by Start again, newest first, so the person can switch back to them.
const HISTORY_KEY = 'flame-mint-playground-wallets-v1';
const MODES = ['independent', 'service', 'delegating'];

const live = {
  state: null,
  offline: false,
  lastReceived: 0,
  wallet: null,
  walletError: null,
  minter: null,
  btcUsd: null,
  pending: null,
  submitting: false,
  version: 0,
  projectedReward,
  contributionFee,
};
window.FlameLive = live;

const changed = () => { live.version++; window.dispatchEvent(new Event('flamelive')); };
const post = async (path, body) => {
  const response = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, signal: AbortSignal.timeout(10000), body: JSON.stringify(body) });
  const result = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(result.error || `Server returned ${response.status}`);
  return result;
};

live.ownUserId = () => live.minter?.user_id ?? live.state?.minters?.find(account => account.address === live.wallet?.address)?.user_id ?? null;
// Without a saved rate a new Service contribution starts at 2%.
live.serviceCommissionBps = () => live.minter?.service_commission_bps ?? live.state?.services?.find(service => service.user_id === live.ownUserId())?.commission_bps ?? null;
live.connected = () => !!live.state && !live.offline && Date.now() - live.lastReceived <= 5000;

async function poll() {
  try {
    const response = await fetch('/api/state?full_history=true', { cache: 'no-store', signal: AbortSignal.timeout(10000) });
    if (!response.ok) throw new Error(`Server returned ${response.status}`);
    live.state = await response.json();
    live.lastReceived = Date.now();
    live.offline = false;
  } catch {
    live.offline = true;
  } finally {
    changed();
    setTimeout(poll, 1000);
  }
}

async function refreshQuote() {
  try {
    const response = await fetch('https://api.coinbase.com/v2/prices/BTC-USD/spot', { credentials: 'omit', referrerPolicy: 'no-referrer', cache: 'no-store', signal: AbortSignal.timeout(10000) });
    if (!response.ok) throw new Error(`Quote returned ${response.status}`);
    live.btcUsd = parseBtcUsdQuote(await response.json());
  } catch {
    live.btcUsd = null;
  } finally {
    changed();
    setTimeout(refreshQuote, 60000);
  }
}

async function loadMinter(fields = {}) {
  const address = live.wallet?.address;
  if (!address) throw new Error('Wallet not ready');
  const result = await post('/api/minter', { address, ...fields });
  if (!Number.isSafeInteger(result.user_id) || result.user_id < 1 || typeof result.name !== 'string') throw new Error('Invalid minter profile');
  if (live.wallet?.address !== address) return null;
  live.minter = { user_id: result.user_id, name: result.name, service_commission_bps: result.service_commission_bps ?? null };
  changed();
  return live.minter;
}

live.prepareWallet = async () => {
  live.wallet = null; live.minter = null; live.walletError = null;
  changed();
  try {
    const stored = JSON.parse(localStorage.getItem(WALLET_KEY) || 'null');
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
      live.wallet = await Promise.race([load(), new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error('The browser wallet did not finish loading.')), 15000);
      })]);
    } finally { clearTimeout(timeout); }
    // Saving must succeed before accepting contributions for this identity.
    localStorage.setItem(WALLET_KEY, JSON.stringify(live.wallet));
  } catch (error) {
    live.wallet = null;
    live.walletError = error.message || String(error);
  }
  changed();
  if (live.wallet) loadMinter().catch(() => {});
};

live.previousWallets = () => {
  try {
    const list = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]');
    return Array.isArray(list) ? list.filter(wallet => typeof wallet?.address === 'string' && typeof wallet?.mnemonic === 'string') : [];
  } catch { return []; }
};

// Puts `keep` at the top of the history and drops `take` from it.
const updateHistory = (keep, take) => {
  const list = live.previousWallets().filter(wallet => wallet.address !== keep?.address && wallet.address !== take);
  if (keep) list.unshift({ address: keep.address, mnemonic: keep.mnemonic });
  localStorage.setItem(HISTORY_KEY, JSON.stringify(list));
};

live.resetWallet = () => {
  try { updateHistory(live.wallet); localStorage.removeItem(WALLET_KEY); } catch { /* A new wallet is created anyway. */ }
  return live.prepareWallet();
};

live.switchWallet = address => {
  const target = live.previousWallets().find(wallet => wallet.address === address);
  if (!target) throw new Error('This wallet is no longer saved');
  updateHistory(live.wallet, address);
  localStorage.setItem(WALLET_KEY, JSON.stringify(target));
  return live.prepareWallet();
};

live.saveName = async name => {
  name = String(name ?? '').trim();
  if (!name || [...name].length > 64 || /[\u0000-\u001f\u007f-\u009f]/.test(name)) throw new Error('Use 1–64 characters without control characters.');
  if (name === live.minter?.name) return live.minter;
  return loadMinter({ name });
};

// Applies to every past and future allocation of this identity's delegators.
live.saveCommission = async bps => {
  if (!Number.isInteger(bps) || bps < 0 || bps > 10000) throw new Error('Use 0–100%, with at most two decimal places.');
  return loadMinter({ service_commission_bps: bps });
};

live.validDraft = draft => [draft.budget_sats, draft.duration, draft.delay_blocks].every(Number.isSafeInteger)
  && draft.budget_sats >= BUDGET_MIN && draft.duration >= DURATION_MIN && draft.delay_blocks >= 0
  && draft.budget_sats >= draft.duration * (contributionFee(draft) + 1)
  && MODES.includes(draft.mode)
  && (draft.mode !== 'delegating' || live.state?.services?.some(service => service.user_id === draft.service_user_id));

// A request is saved before it is sent and resent unchanged until accepted, so a retry never
// creates a second contribution.
live.submit = async draft => {
  if (live.submitting) throw new Error('Already submitting');
  if (!live.wallet) throw new Error('Wallet not ready');
  if (!live.pending) {
    if (!live.validDraft(draft)) throw new Error('Invalid contribution');
    const request = { address: live.wallet.address, budget_sats: draft.budget_sats, duration: draft.duration, delay_blocks: draft.delay_blocks, request_id: crypto.randomUUID() };
    if (draft.mode !== 'independent') request.mode = draft.mode;
    if (draft.mode === 'delegating') request.service_user_id = draft.service_user_id;
    localStorage.setItem(PENDING_KEY, JSON.stringify(request));
    live.pending = request;
  }
  live.submitting = true;
  changed();
  try {
    const result = await post('/api/contributions', live.pending);
    localStorage.removeItem(PENDING_KEY);
    live.pending = null;
    if (live.minter) live.minter.user_id = result.user_id;
    return result;
  } finally {
    live.submitting = false;
    changed();
  }
};

live.discardPending = () => {
  localStorage.removeItem(PENDING_KEY);
  live.pending = null;
  changed();
};

try {
  const saved = JSON.parse(localStorage.getItem(PENDING_KEY) || 'null');
  if (saved && typeof saved.address === 'string' && typeof saved.request_id === 'string' && Number.isSafeInteger(saved.budget_sats) && Number.isSafeInteger(saved.duration)) live.pending = { delay_blocks: 0, ...saved };
} catch { /* An unreadable request is ignored here; the classic Mint page can discard it. */ }

poll();
refreshQuote();
live.prepareWallet();
