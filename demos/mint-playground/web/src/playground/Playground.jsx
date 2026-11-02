// The Mint Playground screen: Network, Mint, Portfolio and Settings tabs over the live engine
// state (live.js). The design's logic class, ported: the state and the imperative pieces (timers,
// per-snapshot caches, the chart's measured height) live here; `vals()` derives everything the
// views in views.jsx show.
import React from 'react';
import { live } from '../live.js';
import { PlaygroundScreen } from './views.jsx';

// Chart windows: 1, 3, 7, 14 and 31 days of blocks.
const ZOOMS = [144, 432, 1008, 2016, 4464];
const DAY = 144;

function initialState() {
  let mpTime = 'days',
    mpUnit = 'usd';
  try {
    if (localStorage.getItem('flame.mpTime') === 'blocks') mpTime = 'blocks';
  } catch {
    /* the default then */
  }
  try {
    const saved = localStorage.getItem('flame.mpUnit');
    if (['usd', 'btc', 'sat'].includes(saved)) mpUnit = saved;
  } catch {
    /* the default then */
  }
  return {
    mpTime,
    mpUnit,
    mpTab: 'live',
    mpMode: 'minter',
    mpAccept: false,
    mpPSort: 'fee',
    mpPDir: 1,
    mpBudget: 10000000,
    mpBTxt: null,
    mpDays: 3,
    mpDTxt: null,
    mpDelay: 0,
    mpDelayTxt: null,
    mpAdvOpt: false,
    mpZoom: 1,
    mpPhase: 'draft',
    mpMinter: null,
    mpCommTxt: null,
    mpName: null,
    mpHi: null,
    mpAllM: false,
    mpMoreC: false,
    mpMSort: null,
    mpSub: null,
    mpBurnId: null,
    mpToast: false,
    mpToastT1: '',
    mpConfirm: false,
    mpResetAsk: false,
    mpWinkId: null,
    mpSubmitMsg: '',
    // Whether the tab's content continues below the visible area (the fade above the tab bar).
    mpMore: false,
    // Bumped on every live-engine change so the derived values are recomputed.
    liveVersion: 0,
  };
}

const num = (n) => n.toLocaleString('en-US');

export default class Playground extends React.Component {
  state = initialState();
  scroller = React.createRef();
  mpRefB = React.createRef();
  mpRefD = React.createRef();
  mpRefS = React.createRef();

  componentDidMount() {
    this.unsubscribe = live.subscribe(() => this.setState({ liveVersion: live.version }));
    // The scroll hint: checked on scroll and on a timer (content grows on its own as snapshots arrive).
    this.hintTimer = setInterval(() => this.scrollHints(), 300);
    this.onResize = () => this.scrollHints();
    addEventListener('resize', this.onResize);
  }

  componentDidUpdate() {
    this.scrollHints();
  }

  componentWillUnmount() {
    this.unsubscribe();
    clearInterval(this.hintTimer);
    removeEventListener('resize', this.onResize);
    for (const timer of [this._mpToastT, this._mpWinkT]) clearTimeout(timer);
  }

  scrollHints() {
    const sc = this.scroller.current;
    const more = !!(sc && sc.scrollHeight - sc.clientHeight - sc.scrollTop > 4);
    if (more !== this.state.mpMore) this.setState({ mpMore: more });
  }

  mpToTop() {
    const sc = this.scroller.current;
    if (sc) sc.scrollTop = 0;
  }

  // Live engine state (live.js): contributions in the design's shape, keyed by 'you' or 'u<user id>'.
  mpLive() {
    return live;
  }
  mpData() {
    const L = this.mpLive(),
      v = L ? L.version : -1;
    if (this._mpD && this._mpD.v === v) return this._mpD;
    const st = L && L.state,
      youId = L ? L.ownUserId() : null,
      idOf = (u) => (u == null ? null : u === youId ? 'you' : 'u' + u),
      fee = st ? st.fee_per_block : 1000;
    const det = new Map((st ? st.contributions : []).map((r) => [r.id, r]));
    const cs = (st ? st.chart_contributions || st.contributions : []).map((r) => {
      const mode = r.mode === 'service' ? 'svc' : r.mode === 'delegating' ? 'del' : 'ind';
      return {
        n: r.id,
        uid: r.user_id,
        who: idOf(r.user_id),
        a: r.start_block,
        b: r.start_block + r.duration,
        v: (r.budget_sats - r.duration * (mode === 'del' ? 0 : fee)) / r.duration,
        budget: r.budget_sats,
        mode,
        to: idOf(r.service_user_id),
        det: det.get(r.id) || null,
      };
    });
    const sums = new Map((st ? st.minters : []).map((m) => [m.user_id, m])),
      svcs = new Map((st ? st.services : []).map((x) => [x.user_id, x]));
    // Blocks before `now` are committed; a contribution starting at `now` burns in the next block.
    return (this._mpD = { v, st, youId, idOf, cs, sums, svcs, fee, now: st ? st.height + 1 : 0 });
  }
  mpUid(id) {
    return id === 'you' ? this.mpData().youId : parseInt(String(id).slice(1), 10);
  }
  mpM(id) {
    const d = this.mpData(),
      L = this.mpLive(),
      uid = this.mpUid(id),
      sm = d.sums.get(uid),
      sv = d.svcs.get(uid);
    if (uid == null || !isFinite(uid))
      return {
        id,
        uid: null,
        name: (L && L.minter && L.minter.name) || 'You',
        mode: 'ind',
        fee: 0,
        active: false,
        to: null,
        addr: '',
      };
    const last = d.cs.filter((k) => k.uid === uid && k.mode === 'del').sort((p, q) => q.n - p.n)[0];
    return {
      id,
      uid,
      name:
        id === 'you' && L && L.minter
          ? L.minter.name
          : sm
            ? sm.name
            : sv
              ? sv.name
              : 'Minter ' + uid,
      mode: sm
        ? sm.mode === 'service'
          ? 'svc'
          : sm.mode === 'delegating'
            ? 'del'
            : 'ind'
        : sv
          ? 'svc'
          : 'ind',
      fee: (sv ? sv.commission_bps : sm ? sm.commission_bps : 0) / 100,
      active: !!(sv && sv.active),
      to: last ? last.to : null,
      addr: sm ? sm.address : '',
    };
  }
  mpOwn() {
    const d = this.mpData(),
      sm = d.youId != null ? d.sums.get(d.youId) : null;
    return {
      burned: sm ? Number(sm.burned_sats) : 0,
      fees: sm ? Number(sm.fees_sats) : 0,
      earned: sm ? Number(sm.reward_sparks) / 1e8 : 0,
    };
  }
  mpSavedComm() {
    const L = this.mpLive(),
      bps = L ? L.serviceCommissionBps() : null;
    return bps == null ? null : bps / 100;
  }
  mpOwnFee() {
    const v = this.mpSavedComm();
    return v == null ? 2 : v;
  }
  mpPrice() {
    const L = this.mpLive();
    return L && L.btcUsd > 0 ? L.btcUsd : null;
  }
  // USD needs a quote; without one, amounts fall back to sats.
  mpU(u) {
    u = u || this.state.mpUnit;
    return u === 'usd' && !this.mpPrice() ? 'sat' : u;
  }
  mpTop() {
    const d = this.mpData();
    if (!this._mpTop || this._mpTop.v !== d.v) {
      const t = new Map();
      d.cs.forEach((k) => {
        if (k.who !== 'you') t.set(k.who, (t.get(k.who) || 0) + k.v * (k.b - k.a));
      });
      this._mpTop = {
        v: d.v,
        top: [...t.entries()]
          .sort((p, q) => q[1] - p[1])
          .slice(0, 3)
          .map((e) => e[0]),
      };
    }
    return this._mpTop.top;
  }
  mpDur(b) {
    return this.state.mpTime === 'blocks'
      ? num(b) + ' blocks'
      : Math.round((b / DAY) * 10) / 10 + (b === DAY ? ' day' : ' days');
  }
  mpTicks(t0, t1, maxN) {
    const out = [];
    if (this.state.mpTime === 'blocks') {
      const raw = Math.max(50, (t1 - t0) / maxN),
        p = Math.pow(10, Math.floor(Math.log10(raw))),
        step = [1, 2, 2.5, 5, 10].map((k) => k * p).find((k) => k >= raw);
      for (let t = Math.ceil(t0 / step) * step; t <= t1; t += step) out.push({ t, label: num(t) });
      return out;
    }
    const rawD = (t1 - t0) / DAY / maxN,
      stepD =
        [1, 2, 3, 7, 14, 28, 30, 61, 91, 182].find((k) => k >= rawD) || Math.ceil(rawD / 365) * 365,
      st = DAY * stepD;
    for (let t = 1 + Math.ceil((t0 - 1) / st) * st; t <= t1; t += st)
      out.push({ t, label: this.mpDay(t) });
    return out;
  }
  mpDay(b) {
    return new Date(
      Date.UTC(2027, 0, 11) + Math.floor((b - 1) / DAY) * 86400000,
    ).toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone: 'UTC' });
  }
  mpNowLabel(now) {
    return this.state.mpTime === 'blocks' ? num(now) : this.mpDay(now);
  }
  mpToastOut() {
    if (!this.state.mpToast || this.state.mpToast === 'out') return;
    clearTimeout(this._mpToastT);
    this.setState({ mpToast: 'out' });
    this._mpToastT = setTimeout(() => this.setState({ mpToast: false }), 220);
  } // fade, then remove
  mpGrp(id) {
    return id === 'you' ? 'you' : this.mpTop().includes(id) ? id : 'others';
  }
  mpCol(id) {
    if (id === 'you') return '#ff3b1f';
    const i = this.mpTop().indexOf(id);
    return i >= 0 ? ['#a1a1a9', '#7c7c84', '#5a5a61'][i] : '#2f2f34';
  }
  mpName(id) {
    const m = this.mpM(id);
    return m ? m.name : id;
  }
  mpFmt(n, u) {
    u = this.mpU(u);
    if (n == null || !isFinite(n)) return '—';
    if (u === 'sat') return (n === 0 ? '0' : num(Math.round(n))) + ' sat';
    if (u === 'usd') {
      const v = (n / 1e8) * this.mpPrice(),
        d = Math.abs(v) >= 10 ? 0 : 2;
      return (
        '$' + v.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d })
      );
    } // whole dollars from $10
    if (n === 0) return '0 BTC';
    const b = n / 1e8;
    return (b >= 1 ? b.toFixed(2) : b.toFixed(4)) + ' BTC';
  }
  mpFmtShort(n, u) {
    u = this.mpU(u);
    if (u === 'sat') return n >= 1000 ? Math.round(n / 1000) + 'k' : String(Math.round(n));
    if (u === 'usd') {
      const v = (n / 1e8) * this.mpPrice();
      return '$' + (v >= 10 ? Math.round(v) : +v.toFixed(v >= 1 ? 1 : 2));
    }
    return String(+(n / 1e8).toPrecision(2));
  }
  mpFmtPrice(satPerFlm, u, pr) {
    u = this.mpU(u);
    pr = pr || 2;
    if (!isFinite(satPerFlm)) return '—';
    if (u === 'sat') return num(Math.round(satPerFlm)) + ' sat/FLM';
    if (u === 'usd') return ((satPerFlm / 1e8) * this.mpPrice()).toPrecision(pr) + ' $/FLM';
    return (satPerFlm / 1e8).toPrecision(pr) + ' BTC/FLM';
  }
  mpUsdRate(satPerFlm) {
    const v = (satPerFlm / 1e8) * this.mpPrice();
    return (v >= 0.1 ? v.toFixed(2) : String(+v.toPrecision(2))) + ' $/FLM';
  }
  mpCompact(n) {
    const u = this.mpU(),
      k = (v) =>
        v >= 1e6
          ? (v / 1e6).toFixed(1) + 'M'
          : v >= 1e3
            ? (v / 1e3).toFixed(1) + 'k'
            : v.toFixed(v >= 10 ? 0 : 2);
    if (u === 'usd') return '$' + k((n / 1e8) * this.mpPrice());
    if (u === 'sat') return k(n);
    return (n / 1e8).toPrecision(3);
  }
  mpFmtIn(n) {
    const u = this.mpU();
    if (!n) return '';
    if (u === 'sat') return num(Math.round(n));
    if (u === 'usd') {
      const v = (n / 1e8) * this.mpPrice(),
        d = Math.abs(v) >= 10 ? 0 : 2;
      return v.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d });
    }
    return String(+(n / 1e8).toFixed(8));
  }
  mpParse(v) {
    const n = parseFloat(String(v).replace(/[,$\s]/g, ''));
    if (!isFinite(n)) return 0;
    const u = this.mpU();
    return Math.round(u === 'sat' ? n : u === 'usd' ? (n / this.mpPrice()) * 1e8 : n * 1e8);
  }
  mpIsActive(id) {
    const m = this.mpM(id);
    return !!(m && m.active);
  }
  mpCalc() {
    const s = this.state,
      d = this.mpData(),
      L = this.mpLive(),
      now = d.now,
      blocks = Math.max(1, Math.round(s.mpDays * DAY)),
      delay = Math.max(0, Math.round(s.mpDelay * DAY)),
      wantDel = s.mpMode === 'delegating';
    // Only minters whose own burns cover every block of this delegation: an inactive one would burn the budget without rewards.
    const from0 = now + delay,
      to0 = from0 + blocks,
      covers = (id) => {
        let t = from0;
        for (const k of d.cs
          .filter((k) => k.who === id && k.mode === 'svc')
          .sort((p, q) => p.a - q.a)) {
          if (k.a > t) break;
          t = Math.max(t, k.b);
          if (t >= to0) return true;
        }
        return false;
      };
    const pids = [...d.svcs.values()]
      .sort((p, q) => p.commission_bps - q.commission_bps || p.user_id - q.user_id)
      .map((x) => d.idOf(x.user_id))
      .filter(covers);
    const selId = pids.includes(s.mpMinter) ? s.mpMinter : pids[0] || null,
      del = wantDel,
      sel = selId ? this.mpM(selId) : null;
    const fees = del ? 0 : d.fee * blocks,
      budget = s.mpBudget || 0,
      burn = budget - fees,
      per = burn > 0 ? burn / blocks : 0;
    const startAbs = now + delay,
      endAbs = startAbs + blocks,
      comm = del && sel ? sel.fee : 0;
    const draft = {
      budget_sats: budget,
      duration: blocks,
      delay_blocks: delay,
      start_block: startAbs,
      user_id: d.youId,
      mode: del ? 'delegating' : s.mpAccept ? 'service' : 'independent',
      commission_bps: del && sel ? Math.round(sel.fee * 100) : 0,
      service_user_id: del && sel ? sel.uid : null,
    };
    let err = '';
    const minB = del ? blocks : blocks * (d.fee + 1);
    if (budget > 0 && budget < minB)
      err =
        'Not enough to cover ' +
        (del ? '1 sat per block' : 'fees') +
        '. Minimum for ' +
        this.mpDur(blocks) +
        ': ' +
        (this.mpU() === 'btc' ? +(minB / 1e8).toFixed(8) + ' BTC' : this.mpFmt(minB)) +
        '.';
    else if (budget > 0 && budget < 1e5) err = 'Minimum budget: ' + this.mpFmt(1e5) + '.';
    const reward =
      L && d.st && per > 0 && !err && !(del && !sel)
        ? L.projectedReward(
            d.cs.map((k) => ({
              budget_sats: k.budget,
              duration: k.b - k.a,
              start_block: k.a,
              user_id: k.uid,
              mode: k.mode === 'svc' ? 'service' : k.mode === 'del' ? 'delegating' : 'independent',
              service_user_id: k.to == null ? null : this.mpUid(k.to),
            })),
            draft,
          ) / 1e8
        : 0;
    const own = this.mpOwn();
    return {
      now,
      blocks,
      delay,
      fees,
      budget,
      burn,
      per,
      startAbs,
      endAbs,
      reward,
      err,
      sel,
      del,
      comm,
      draft,
      earned: own.earned,
      burnedSoFar: own.burned,
      feesSoFar: own.fees,
      share: (reward / (50 * blocks)) * 100,
      cs: d.cs,
      pids,
      selId,
    };
  }
  // Lifetime amounts come from the server; only burn rates over time are derived from the schedules.
  mpNet(c) {
    const d = this.mpData();
    if (this._mpNet && this._mpNet.v === d.v) return this._mpNet;
    const cs = c.cs,
      earn = new Map(),
      svcOf = (k) => (k.mode === 'del' ? k.to : null),
      fee = d.fee;
    cs.forEach((k) => earn.set(k, k.det ? Number(k.det.reward_sparks) / 1e8 : 0));
    const burnedAt = (t) => cs.reduce((x, k) => x + k.v * Math.max(0, Math.min(t, k.b) - k.a), 0),
      feesAt = (t) =>
        cs.reduce((x, k) => x + (svcOf(k) ? 0 : fee * Math.max(0, Math.min(t, k.b) - k.a)), 0),
      burnAt = (t) => cs.reduce((x, k) => x + (t >= k.a && t < k.b ? k.v : 0), 0);
    return (this._mpNet = { v: d.v, earn, burnedAt, feesAt, burnAt });
  }
  // FLM issued before block t: the lifetime total minus 50 FLM for each later committed block that had an eligible burn.
  mpIssued(t) {
    const d = this.mpData(),
      total = d.st ? Number(d.st.totals.reward_sparks) / 1e8 : 0;
    if (t >= d.now) return total;
    const lo = Math.max(1, t),
      cs = d.cs.filter((k) => k.b > lo && k.a < d.now),
      pts = [
        ...new Set([
          lo,
          d.now,
          ...cs.flatMap((k) => [k.a, k.b]).filter((x) => x > lo && x < d.now),
        ]),
      ].sort((p, q) => p - q);
    let n = 0;
    for (let i = 0; i < pts.length - 1; i++) {
      const mid = (pts[i] + pts[i + 1]) / 2,
        on = (k) => k.a <= mid && k.b > mid;
      if (
        cs.some(
          (k) =>
            on(k) &&
            (k.mode !== 'del' || cs.some((o) => o.who === k.to && o.mode === 'svc' && on(o))),
        )
      )
        n += pts[i + 1] - pts[i];
    }
    return Math.max(0, total - 50 * n);
  }
  mpNice(x) {
    const p = Math.pow(10, Math.floor(Math.log10(x))),
      f = x / p;
    return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10) * p;
  }
  mpDraft(c) {
    return c.per > 0 && !c.err
      ? [
          {
            who: c.cs.some((k) => k.who === 'you') ? 'new' : 'you',
            a: c.startAbs,
            b: c.endAbs,
            v: Math.round(c.per),
          },
        ]
      : [];
  }
  mpWin(c) {
    const F = ZOOMS[this.state.mpZoom],
      P = Math.min(Math.max(Math.round(F * 0.3), 72), 864);
    return { t0: c.now - P, t1: c.now + F + Math.round(F * 0.05) };
  }
  mpMintChart(c) {
    const w = this.mpWin(c);
    return this.mpBandChart(c, w.t0, w.t1, this.mpDraft(c), 'mc', this.mpFitChart());
  }
  // The Mint tab should fit one screen so Confirm is always in sight: on shorter screens the chart gives up height.
  // It depends on the screen only (553 is the rest of the Mint tab), so Network's chart matches and nothing jumps between tabs.
  mpFitChart() {
    const sc = this.scroller.current,
      h = this._mcH ?? 100;
    if (sc && sc.clientHeight) {
      const fit = Math.max(48, Math.min(100, Math.floor(sc.clientHeight - 553)));
      if (fit !== h) {
        this._mcH = fit;
        requestAnimationFrame(() => this.setState({}));
      }
    }
    return this._mcH ?? 100;
  }
  mpLiveChart(c) {
    const w = this.mpWin(c);
    return this.mpBandChart(c, w.t0, w.t1, [], 'lc', this.mpFitChart());
  }
  mpBandChart(c, t0, t1, draft, cid, H = 100) {
    const s = this.state,
      L = 48,
      PW = 302,
      Y0 = 8,
      X = (t) => L + ((t - t0) / (t1 - t0)) * PW;
    const cs = c.cs.concat(draft).filter((k) => k.b > t0 && k.a < t1),
      tot = new Map();
    const wn =
        cid === 'mc' && s.mpWinkId != null && cs.some((k) => k.n === s.mpWinkId && k.who === 'you')
          ? s.mpWinkId
          : null,
      gk = (k) => (k.n === wn ? 'wink' : k.who); // a fresh burn grows in on its own, on the Mint chart only
    cs.forEach((k) => {
      const g = gk(k);
      if (g !== 'you' && g !== 'new' && g !== 'wink')
        tot.set(g, (tot.get(g) || 0) + k.v * (Math.min(k.b, t1) - Math.max(k.a, t0)));
    });
    const fst = (g) => Math.min(...cs.filter((k) => gk(k) === g).map((k) => k.a)),
      order = [
        ...[...tot.entries()].sort((p, q) => fst(p[0]) - fst(q[0]) || q[1] - p[1]).map((e) => e[0]),
        'you',
        'wink',
        'new',
      ];
    const pts = [
        ...new Set([t0, t1, ...cs.flatMap((k) => [Math.max(t0, k.a), Math.min(t1, k.b)])]),
      ].sort((p, q) => p - q),
      segs = [];
    for (let i = 0; i < pts.length - 1; i++) {
      const p = pts[i],
        q = pts[i + 1],
        mid = (p + q) / 2;
      segs.push({
        p,
        q,
        am: order.map((g) =>
          cs.filter((k) => gk(k) === g && k.a <= mid && k.b > mid).reduce((x, k) => x + k.v, 0),
        ),
      });
    }
    const top = this.mpYTop(c) * 50,
      Y = (v) => H - (v / top) * (H - Y0);
    segs.forEach((g) => {
      let acc = 0;
      g.band = g.am.map((v) => {
        const lo = acc;
        acc += v;
        return v > 0 ? [Y(acc), Y(lo)] : null;
      });
    });
    const nx = X(c.now),
      kids = [],
      fills = [],
      lines = [];
    order.forEach((g, m) => {
      let run = [];
      const flush = () => {
        if (!run.length) return;
        const tp = [],
          bt = [];
        run.forEach((sg) => {
          const x0 = X(sg.p).toFixed(1),
            x1 = X(sg.q).toFixed(1),
            [yt, yb] = sg.band[m];
          tp.push(x0 + ' ' + yt.toFixed(1), x1 + ' ' + yt.toFixed(1));
          bt.unshift(x1 + ' ' + yb.toFixed(1), x0 + ' ' + yb.toFixed(1));
        });
        if (g === 'wink') {
          run.forEach((sg) => {
            const x0 = X(sg.p).toFixed(1),
              x1 = X(sg.q).toFixed(1),
              [yt, yb] = sg.band[m];
            fills.push({
              d:
                'M' +
                x0 +
                ' ' +
                yt.toFixed(1) +
                'L' +
                x1 +
                ' ' +
                yt.toFixed(1) +
                'L' +
                x1 +
                ' ' +
                yb.toFixed(1) +
                'L' +
                x0 +
                ' ' +
                yb.toFixed(1) +
                'Z',
              g,
              fill: '#ff3b1f',
              op: 1,
              at: sg.p,
            });
          });
          run = [];
          return;
        }
        const d = 'M' + tp.concat(bt).join('L') + 'Z';
        fills.push({
          d,
          g,
          fill:
            g === 'you' || g === 'new'
              ? '#ff3b1f'
              : ['#ff9922', '#ffaf01', '#ffc101', '#ffcd00', '#ffd800', '#ffe400', '#fef100'][
                  m % 7
                ],
          op: g === 'new' ? 0.4 : 1,
        });
        lines.push({ d, stroke: g === 'you' || g === 'new' ? '#ff9a80' : '#c7c7cc' });
        run = [];
      };
      segs.forEach((sg) => {
        if (sg.band[m] && (!run.length || run[run.length - 1].q === sg.p)) run.push(sg);
        else {
          flush();
          if (sg.band[m]) run.push(sg);
        }
      });
      flush();
    });
    const txt = {
      fontSize: 12,
      fill: 'rgba(255,255,255,0.7)',
      fontFamily: 'inherit',
      style: { fontVariantNumeric: 'tabular-nums' },
    };
    [0, top / 2, top].forEach((v, i) => {
      const y = Y(v);
      kids.push(
        <line
          key={'g' + i}
          x1={L}
          x2={L + PW}
          y1={y}
          y2={y}
          stroke={v === 0 ? 'rgba(255,255,255,0.4)' : 'rgba(255,255,255,0.12)'}
          strokeWidth={1}
        />,
        <text key={'y' + i} x={0} y={y + 4} {...txt}>
          {v === 0 ? '0' : this.mpFmtShort(v / 50)}
        </text>,
      );
    });
    kids.push(
      <defs key="defs">
        <clipPath id={cid + 'P'}>
          <rect x={-10} y={-10} width={nx + 10} height={H + 30} />
        </clipPath>
        <clipPath id={cid + 'F'}>
          <rect x={nx} y={-10} width={400 - nx} height={H + 30} />
        </clipPath>
      </defs>,
    );
    if (cid === 'mc' && this._wkFor !== wn) {
      this._wkFor = wn;
      this._wkDelay = {};
      this._wkStart = null;
    }
    const wkDelay = (at) =>
      (this._wkDelay[at] ??= -Math.min(2000, Date.now() - (this._wkStart ??= Date.now()))); // pieces split off later (others' burns add segment edges) join the animation where it already is
    fills.forEach((f, i) =>
      kids.push(
        <path
          key={f.g === 'wink' ? 'w' + wn + '-' + f.at : 'f' + i}
          d={f.d}
          fill={f.fill}
          fillOpacity={f.op}
          stroke={f.op === 1 ? f.fill : 'none'}
          strokeWidth={1}
          strokeLinejoin="miter"
          style={
            f.g === 'wink'
              ? {
                  transformBox: 'fill-box',
                  transformOrigin: '50% 100%',
                  animation:
                    'mpGrow 0.5s ease-out ' +
                    wkDelay(f.at) +
                    'ms both, mpFlash 0.8s ease-out ' +
                    (450 + wkDelay(f.at)) +
                    'ms both',
                }
              : undefined
          }
        />,
      ),
    );
    const nowLbl = this.mpNowLabel(c.now);
    this.mpTicks(t0, t1, 4).forEach((k) => {
      const x = X(k.t);
      if (x < L - 0.5 || x > L + PW + 0.5) return;
      kids.push(
        <line
          key={'xt' + k.t}
          x1={x}
          x2={x}
          y1={H}
          y2={H + 5}
          stroke="rgba(255,255,255,0.4)"
          strokeWidth={1}
        />,
      );
      if (Math.abs(x - nx) > 44 && k.label !== nowLbl)
        kids.push(
          <text key={'xl' + k.t} x={x} y={H + 20} textAnchor="middle" {...txt}>
            {k.label}
          </text>,
        );
    });
    kids.push(
      <text
        key="nowv"
        x={nx}
        y={H + 20}
        textAnchor="middle"
        {...txt}
        fontWeight={600}
        fill="#ffffff"
      >
        {nowLbl}
      </text>,
    );
    kids.push(
      <line
        key="now"
        x1={nx}
        x2={nx}
        y1={0}
        y2={H + 6}
        stroke="rgba(255,255,255,0.5)"
        strokeWidth={1.5}
      />,
    );
    kids.push(
      <text
        key="nowl"
        x={nx}
        y={-6}
        textAnchor="middle"
        fontSize={12}
        fontWeight={600}
        fill="#ffffff"
        fontFamily="inherit"
      >
        Now
      </text>,
    );
    return (
      <svg
        width={350}
        height={H + 26}
        viewBox={'0 0 350 ' + (H + 26)}
        style={{ position: 'absolute', left: 0, top: 0, overflow: 'visible' }}
      >
        {kids}
      </svg>
    );
  }
  mpYTop(c) {
    const w = this.mpWin(c),
      all = c.cs.concat(this.mpDraft(c)).filter((k) => k.b > w.t0 && k.a < w.t1),
      bp = [
        ...new Set([w.t0, w.t1, ...all.flatMap((k) => [Math.max(w.t0, k.a), Math.min(w.t1, k.b)])]),
      ].sort((p, q) => p - q);
    let mx = 1000;
    for (let i = 0; i < bp.length - 1; i++) {
      const mid = (bp[i] + bp[i + 1]) / 2;
      mx = Math.max(
        mx,
        all.reduce((x, k) => x + (k.a <= mid && k.b > mid ? k.v : 0), 0),
      );
    }
    return this.mpNice((Math.max(mx / 50, 1) * 1.08) / 2) * 2;
  }
  // A drag along a slider track; `set` gets the fraction 0–1 of its width.
  mpDown = (ref, set) => (ev) => {
    const el = ref.current;
    if (!el) return;
    const upd = (e) => {
      const r = el.getBoundingClientRect();
      set(Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)));
    };
    upd(ev);
    const up = () => {
      window.removeEventListener('pointermove', upd);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', upd);
    window.addEventListener('pointerup', up);
  };
  mpDownB = this.mpDown(this.mpRefB, (f) =>
    this.setState({ mpBudget: Math.round((1e5 + f * (1e8 - 1e5)) / 1e5) * 1e5, mpBTxt: null }),
  );
  mpDownD = this.mpDown(this.mpRefD, (f) =>
    this.setState({ mpDays: Math.round(1 + f * 30), mpDTxt: null }),
  );
  mpDownS = this.mpDown(this.mpRefS, (f) =>
    this.setState({ mpDelay: Math.round(f * 31), mpDelayTxt: null }),
  );
  mpModeTabs(o) {
    // Mint tab: Independent / Delegating as tabs over one panel, like the tutorial
    const del = !!o.mpIsDel,
      on = '#ff3b1f',
      off = 'rgba(255,255,255,0.35)';
    return {
      mtMinBg: del ? 'transparent' : '#1a1a1d',
      mtDelBg: del ? '#1a1a1d' : 'transparent',
      mtMinFg: del ? 'rgba(255,255,255,0.6)' : '#ffffff',
      mtDelFg: del ? '#ffffff' : 'rgba(255,255,255,0.6)',
      mtMinW: del ? 1.5 : 6,
      mtMinC: del ? off : on,
      mtDelW: del ? 6 : 1.5,
      mtDelC: del ? on : off,
      mtRadius: del ? '16px 0 16px 16px' : '0 16px 16px 16px',
      mtCommOp: o.mpAccept ? 1 : 0.35,
      mtCommPe: o.mpAccept ? 'auto' : 'none',
      mtCommOff: !o.mpAccept,
    };
  }
  // Everything the views show, derived from the state and the live engine.
  vals() {
    const s = this.state,
      c = this.mpCalc(),
      u = this.mpU(),
      L = this.mpLive(),
      d = this.mpData(),
      o = {},
      now = c.now,
      net = this.mpNet(c);
    const mine = c.cs.filter((k) => k.who === 'you');
    o.mpOfflineBanner = !!(L && L.offline);
    // stats
    const fl1 = this.mpIssued(now),
      bn = net.burnedAt(now);
    o.mpS1 = { value: Math.floor(fl1).toLocaleString('en-US') + ' FLM' };
    o.mpS2 = { value: this.mpFmt(d.st ? Number(d.st.totals.burned_sats) : bn) };
    // your stake
    const yv = mine.reduce((x, k) => x + (k.a <= now && k.b > now ? k.v : 0), 0),
      yOn = yv > 0;
    // minters
    const ids = [...new Set(c.cs.map((k) => k.who))],
      mStats = (id) => {
        const sm = d.sums.get(this.mpUid(id)),
          burned = sm ? Number(sm.burned_sats) : 0,
          fees = sm ? Number(sm.fees_sats) : 0,
          earned = sm ? Number(sm.reward_sparks) / 1e8 : 0;
        return { burned, fees, earned, rate: earned > 0 ? burned / earned : NaN };
      };
    const badgeOf = (m) =>
      m.mode === 'svc'
        ? 'Minter · ' + m.fee + '% fee'
        : m.mode === 'del'
          ? 'Delegate → ' + this.mpName(m.to)
          : 'Independent';
    const ml = ids.map((id) => ({ id, m: this.mpM(id), ...mStats(id) }));
    const sorters = {
      earned: (p, q) => q.earned - p.earned,
      burned: (p, q) => q.burned - p.burned,
      rate: (p, q) => (p.rate || 1e18) - (q.rate || 1e18),
      name: (p, q) => p.m.name.localeCompare(q.m.name),
    };
    ml.sort(sorters[s.mpMSort] || sorters.burned); // a tap on a column header sorts by it
    const mCard = (x) => ({
      burnedC: this.mpCompact(x.burned),
      earnedN:
        x.earned >= 1e5
          ? Math.round(x.earned / 1000) + 'k'
          : Math.floor(x.earned).toLocaleString('en-US'),
      rateN: this.mpFmtPrice(x.rate, u === 'btc' ? 'sat' : u).replace(/ .*$/, ''),
      name: x.m.name,
      you: x.id === 'you',
      badge: badgeOf(x.m),
      bg: x.id === 'you' ? 'rgba(255,255,255,0.08)' : s.mpHi === x.id ? '#26262b' : 'transparent',
    });
    const minterCards = (s.mpAllM ? ml : ml.slice(0, 5)).map(mCard);
    o.mpNMinters = ml.length;
    o.mpRateHdr = u === 'usd' ? '$/FLM' : 'sat/FLM';
    {
      const by = s.mpMSort || 'burned';
      [
        ['Name', 'name'],
        ['Burned', 'burned'],
        ['Earned', 'earned'],
        ['Rate', 'rate'],
      ].forEach(([k, v]) => {
        o['mpSortBy' + k] = () => this.setState({ mpMSort: v });
        o['mpSort' + k + 'Fg'] = by === v ? '#ffffff' : 'inherit';
        o['mpSort' + k + 'W'] = by === v ? 600 : 400;
      });
    }
    // contributions
    const ago = (b) => {
      const m = b * 10;
      return m >= 1440
        ? Math.round(m / 1440) + ' d'
        : m >= 60
          ? Math.round(m / 60) + ' h'
          : m + ' min';
    };
    const cCard = (k) => {
      const m = { ...this.mpM(k.who), mode: k.mode, to: k.to },
        on = k.a <= now && k.b > now,
        fut = k.a > now,
        pct = Math.round(((now - k.a) / (k.b - k.a)) * 100),
        budget = k.budget;
      const amt = this.mpCompact(budget) + (u === 'sat' ? ' sat' : u === 'btc' ? ' BTC' : '');
      return {
        role: badgeOf(m),
        when: fut
          ? this.state.mpTime === 'blocks'
            ? num(k.a) + ' – ' + num(k.b - 1)
            : this.mpDay(k.a) === this.mpDay(k.b - 1)
              ? this.mpDay(k.a)
              : this.mpDay(k.a) + ' – ' + this.mpDay(k.b - 1)
          : Math.floor(net.earn.get(k) || 0).toLocaleString('en-US') + ' FLM',
        title: m.name,
        you: k.who === 'you',
        active: on,
        pct,
        status: on ? 'Active' : fut ? 'Planned' : 'Ended',
        sFg: on ? '#30d158' : 'rgba(255,255,255,0.7)',
        sub: amt,
        bg:
          k.who === 'you' ? 'rgba(255,255,255,0.08)' : s.mpHi === k.who ? '#26262b' : 'transparent',
      };
    };
    const act = c.cs.filter((k) => k.a <= now && k.b > now).sort((p, q) => q.v - p.v),
      sch = c.cs.filter((k) => k.a > now).sort((p, q) => p.a - q.a),
      fin = c.cs.filter((k) => k.b <= now).sort((p, q) => q.b - p.b);
    const top = act
        .slice()
        .sort((p, q) => q.a - p.a)
        .concat(sch, fin),
      contribs = (s.mpMoreC ? top : top.slice(0, 5)).map(cCard);
    o.mpHasContribs = top.length > 0;
    o.mpNContribs = top.length;
    o.mpNoStakes = act.length === 0 && sch.length === 0;
    o.mpNoStakesText = L && L.state ? 'Nothing is burning right now' : 'Loading…';
    // mint
    o.mpNoProvText =
      L && L.state
        ? 'No minters found that mint for the next ' + this.mpDur(c.blocks)
        : 'Loading minters…';
    o.mpSetModeMin = () => this.setState({ mpMode: 'minter' });
    o.mpSetModeDel = () => this.setState({ mpMode: 'delegating' });
    o.mpIsMinter = !c.del;
    o.mpIsDel = c.del;
    o.mpAccept = s.mpAccept;
    o.mpToggleAccept = () => this.setState((t) => ({ mpAccept: !t.mpAccept }));
    o.mpAccBg = s.mpAccept ? '#30d158' : '#3a3a3f';
    o.mpAccX2 = s.mpAccept ? 20 : 2;
    o.mpCommVal = s.mpCommTxt ?? String(this.mpOwnFee());
    o.mpOnComm = (e) => {
      let v = e.target.value
        .replace(',', '.')
        .replace(/[^\d.]/g, '')
        .replace(/^(\d*\.\d{0,2}).*$/, '$1');
      if (parseFloat(v) > 100) v = '100';
      this.setState({ mpCommTxt: v });
    };
    o.mpBlurComm = () => {
      if (!String(this.state.mpCommTxt ?? 'x').trim()) this.setState({ mpCommTxt: null });
    };
    o.mpFocusEnd = (e) => {
      const t = e.target;
      requestAnimationFrame(() => t.setSelectionRange(t.value.length, t.value.length));
    };
    o.mpCommW = Math.max(2, String(o.mpCommVal).length + 0.6);
    o.mpBudgetVal = s.mpBTxt ?? this.mpFmtIn(s.mpBudget);
    o.mpOnBudget = (e) => {
      const v = e.target.value,
        n = this.mpParse(v);
      this.setState({ mpBTxt: n > 1e8 ? null : v, mpBudget: Math.min(1e8, n) });
    };
    o.mpBlurBudget = () => this.setState({ mpBTxt: null });
    o.mpBudgetW = Math.max(3, String(o.mpBudgetVal).length + 0.6);
    o.mpUnitLabel = u === 'sat' ? 'sat' : u === 'usd' ? 'USD' : 'BTC';
    o.mpPctB = (Math.max(0, Math.min(1, ((s.mpBudget || 0) - 1e5) / (1e8 - 1e5))) * 100).toFixed(1);
    o.mpHasErr = !!c.err;
    o.mpErr = c.err;
    o.mpBudgetRing = c.err ? 'inset 0 0 0 1.5px #ff6b57' : 'none';
    o.mpPctD = (((s.mpDays - 1) / 30) * 100).toFixed(1);
    const ok = c.budget > 0 && !c.err && !(c.del && !c.sel);
    const done = mine.length > 0;
    o.mpConfirmOn = !!s.mpConfirm;
    o.mpConfirmCancel = () => this.setState({ mpConfirm: false });
    o.mpCf = {
      mode: c.del ? 'Delegate → ' + (c.sel ? c.sel.name : '—') : 'Independent',
      later: s.mpDelay > 0,
      burn: this.mpFmt(Math.max(0, c.burn)),
      feeLabel: c.del ? 'Minter commission' : 'Mining fees',
      fees: c.del ? (c.sel ? c.sel.fee : 0) + '%' : this.mpFmt(c.fees),
      total: this.mpFmt(c.budget),
      dur: this.mpDur(c.blocks),
      starts: s.mpDelay > 0 ? this.mpDur(Math.round(s.mpDelay * DAY)) : 'Now',
      startsLabel: s.mpDelay > 0 ? 'Starts in' : 'Starts',
      reward: Math.round(c.reward).toLocaleString('en-US') + ' FLM',
    };
    ['Live', 'Mint', 'Me', 'Set'].forEach((k) => {
      const v = k.toLowerCase();
      o['mpGo' + k] = () =>
        this.setState({ mpTab: v, mpConfirm: false, mpSub: null }, () => this.mpToTop());
      o['mpTabCol' + k] = s.mpTab === v ? '#0a84ff' : '#8e8e93';
      o['mpTabFill' + k] = s.mpTab === v ? 'currentColor' : 'none';
      o['mpTabSw' + k] = s.mpTab === v ? 2.2 : 1.7;
      o['mpTabInner' + k] = s.mpTab === v ? '#141416' : 'none';
    });
    o.mpIsLiveB = s.mpTab === 'live';
    o.mpIsMintB = s.mpTab === 'mint';
    o.mpIsMeB = s.mpTab === 'me';
    o.mpIsSet = s.mpTab === 'set';
    o.mpSubMinters = s.mpSub === 'minters';
    o.mpSubContribs = s.mpSub === 'contribs';
    o.mpCloseSub = () => this.setState({ mpSub: null });
    o.mpOpenMinters = () => this.setState({ mpSub: 'minters', mpAllM: true });
    o.mpOpenContribs = () => this.setState({ mpSub: 'contribs', mpMoreC: true });
    o.mpRefB = this.mpRefB;
    o.mpRefD = this.mpRefD;
    o.mpRefS = this.mpRefS;
    o.mpDownB = this.mpDownB;
    o.mpDownD = this.mpDownD;
    o.mpDownS = this.mpDownS;
    // me
    {
      const prNow = net.burnAt(now) / 50;
      o.mpHasPl = c.earned > 0 && prNow > 0;
      o.mpPl = {
        mkt:
          u === 'usd' && prNow > 0
            ? this.mpUsdRate(prNow)
            : this.mpFmtPrice(prNow, u === 'btc' ? 'sat' : u, 3),
      };
    }
    {
      // Portfolio: every own burn with its own spend, flames and result at today's network mint rate
      const rateNow = net.burnAt(now) / 50,
        numOf = new Map(
          mine
            .slice()
            .sort((p, q) => p.n - q.n)
            .map((k, i) => [k.n, i + 1]),
        ),
        svcMine = mine.filter((k) => k.mode === 'svc'),
        cIn = new Map();
      // Commission received has no per-burn split on the server: spread each delegation's commission over the own service burns that were active with it.
      c.cs
        .filter((k) => k.to === 'you' && k.who !== 'you' && k.det)
        .forEach((d) => {
          const tot = Number(d.det.commission_sparks) / 1e8,
            e = Math.min(d.b, now);
          if (!(tot > 0) || e <= d.a) return;
          const pts = [
              ...new Set([
                d.a,
                e,
                ...svcMine.flatMap((k) => [k.a, k.b]).filter((t) => t > d.a && t < e),
              ]),
            ].sort((x, y) => x - y),
            segs = [];
          for (let i = 0; i < pts.length - 1; i++) {
            const on = svcMine.filter((k) => k.a <= pts[i] && k.b > pts[i]);
            if (on.length) segs.push({ w: pts[i + 1] - pts[i], on });
          }
          const W = segs.reduce((x, g) => x + g.w, 0);
          segs.forEach((g) =>
            g.on.forEach((k) => cIn.set(k.n, (cIn.get(k.n) || 0) + (tot * g.w) / W / g.on.length)),
          );
        });
      const flm = (v) => (v >= 10 ? Math.round(v) : +v.toFixed(2)).toLocaleString('en-US') + ' FLM',
        cost = (v) => (v ? this.mpFmt(v) : this.mpFmt(0).replace(/\.0+(?= |$)/, '')),
        res = (v) => (v < 0 ? '(' + this.mpFmt(-v) + ')' : (v > 0 ? '+' : '') + cost(v)); // spending without minus signs; a loss in parentheses, as in financial statements
      const rate = (v) =>
          !(isFinite(v) && v > 0)
            ? '—'
            : u === 'usd'
              ? this.mpUsdRate(v)
              : this.mpFmtPrice(v, null, 3),
        resFg = (v) => (v > 0 ? '#30d158' : '#ffffff'),
        unit = u === 'usd' ? 'USD' : u === 'btc' ? 'BTC' : 'sat';
      const burns = mine.map((k) => {
        const det = k.det || {},
          burned = Number(det.burned_sats || 0),
          fees = Number(det.fees_sats || 0),
          own = Number(det.reward_sparks || 0) / 1e8,
          paid = Number(det.commission_sparks || 0) / 1e8;
        const del = k.mode === 'del',
          svc = k.mode === 'svc',
          got = svc ? cIn.get(k.n) || 0 : 0,
          total = own + got,
          value = total * rateNow,
          result = value - burned - fees,
          r = cCard(k);
        const type = del ? 'Delegation' : svc ? 'Independent with delegations' : 'Independent',
          minter = del ? this.mpName(k.to) : '';
        const mid = del
          ? [
              { l: 'Flames minted', v: flm(own + paid) },
              {
                l:
                  'Commission (' +
                  (det.commission_bps != null
                    ? det.commission_bps / 100
                    : (this.mpM(k.to)?.fee ?? 0)) +
                  '%)',
                v: '−' + flm(paid),
              },
              { l: 'Flames earned', v: flm(own) },
            ]
          : svc
            ? [
                { l: 'Flames earned', v: flm(own) },
                { l: 'Commission earned (' + this.mpOwnFee() + '%)', v: '+' + flm(got) },
                { l: 'Total flames', v: flm(total) },
              ]
            : [{ l: 'Total flames', v: flm(total) }];
        return {
          k,
          total,
          result,
          burned,
          fees,
          own,
          got,
          row: {
            num: numOf.get(k.n),
            amt: this.mpFmtIn(k.budget) || '0',
            unit,
            kind: (svc ? 'Independent +' : type) + (minter ? ' · ' + minter : ''),
            active: r.active,
            pct: r.pct,
            resNum:
              k.a > now
                ? 'Planned'
                : result < 0
                  ? '(' + (this.mpFmtIn(-result) || '0') + ')'
                  : (result > 0 ? '+' : '') + (this.mpFmtIn(result) || '0'),
            resUnit: k.a > now ? '' : unit,
            resFg: k.a > now ? 'rgba(255,255,255,0.6)' : resFg(result),
            open: () => this.setState({ mpSub: 'burn', mpBurnId: k.n }),
          },
          detail: {
            title: 'Burn #' + numOf.get(k.n),
            sub: type + (minter ? ' · ' + minter : '') + ' · ' + r.status,
            g1: [
              { l: 'Total expenditure', v: cost(burned + fees) },
              { l: 'Mining fees', v: cost(fees) },
              { l: 'Burned', v: cost(burned) },
              ...mid,
              { l: 'Avg mint cost', v: rate(burned / total) },
            ],
            g2: [
              { l: 'Current mint rate', v: rate(rateNow) },
              { l: 'Flames value', v: this.mpFmt(value) },
            ],
            result: res(result),
            resFg: resFg(result),
          },
        };
      });
      const order = [
        ...burns.filter((b) => b.k.a <= now && b.k.b > now).sort((p, q) => q.k.a - p.k.a),
        ...burns.filter((b) => b.k.a > now).sort((p, q) => p.k.a - q.k.a),
        ...burns.filter((b) => b.k.b <= now).sort((p, q) => q.k.b - p.k.b),
      ];
      const accF = burns.reduce((x, b) => x + b.total, 0),
        accR = burns.reduce((x, b) => x + b.result, 0),
        accB = burns.reduce((x, b) => x + b.burned, 0);
      o.mpAvgRate = rate(accB / accF);
      {
        const sum = (f) => burns.reduce((x, b) => x + b[f], 0),
          fe = sum('fees');
        const top = (l, v) => ({
            l,
            v,
            fs: 15,
            fg: '#ffffff',
            lfg: 'rgba(255,255,255,0.7)',
            ind: 0,
            bt: '0.5px solid rgba(255,255,255,0.2)',
            pad: '12px 0 6px',
          }),
          sub = (l, v) => ({
            l,
            v,
            fs: 13,
            fg: 'rgba(255,255,255,0.7)',
            lfg: 'rgba(255,255,255,0.55)',
            ind: 116,
            bt: 'none',
            pad: '2px 0',
          });
        const tail = (rows, last) => {
          rows[rows.length - 1].pad = last ? '2px 0 12px' : rows[rows.length - 1].pad;
          return rows;
        }; // a group ends with a little air before the next line
        o.mpSummary = [
          ...tail(
            [
              top('Total expenditure', cost(accB + fe)),
              sub('Burned', cost(accB)),
              sub('Mining fees', cost(fe)),
            ],
            true,
          ),
          ...tail(
            [
              top('Flames accumulated', flm(accF)),
              sub('From minting', flm(sum('own'))),
              sub('From delegations', flm(sum('got'))),
            ],
            true,
          ),
          { ...top('Avg mint cost', rate(accB / accF)), pad: '12px 0' },
          { ...top('Current mint rate', rate(rateNow)), pad: '12px 0' },
        ];
        Object.assign(o.mpSummary[0], { bt: 'none', pad: '14px 0 6px' });
      } // Network: all burned bitcoin over all accumulated flames, fees excluded like the network rate
      o.mpAccFlm = (accF >= 10 ? Math.round(accF) : +accF.toFixed(2)).toLocaleString('en-US');
      o.mpAccRes =
        accR < 0
          ? '(' + (this.mpFmtIn(-accR) || '0') + ')'
          : (accR > 0 ? '+' : '') + (this.mpFmtIn(accR) || '0');
      o.mpAccUnit = unit;
      o.mpAccResFg = resFg(accR);
      o.mpSumRes = res(accR);
      o.mpHasBurns = burns.length > 0;
      o.mpNoBurns = !burns.length;
      o.mpBurnsIn = order.slice(0, -1).map((b) => b.row);
      o.mpBurnsOut = order.slice(-1).map((b) => b.row);
      const sel = burns.find((b) => b.k.n === s.mpBurnId);
      o.mpSubBurn = s.mpSub === 'burn' && !!sel;
      o.mpBD = sel ? sel.detail : { g1: [], g2: [] };
    }
    {
      const dl = c.cs.filter((k) => k.to === 'you' && k.who !== 'you');
      const rows = [
        ...dl.filter((k) => k.a <= now && k.b > now),
        ...dl.filter((k) => k.a > now),
        ...dl.filter((k) => k.b <= now),
      ].map((k) => {
        const r = cCard(k);
        return {
          ...r,
          when:
            k.a > now
              ? r.when
              : '+' +
                Math.floor(k.det ? Number(k.det.commission_sparks) / 1e8 : 0).toLocaleString(
                  'en-US',
                ) +
                ' FLM to you',
        };
      });
      o.mpDlgIn = rows.slice(0, -1);
      o.mpDlgOut = rows.slice(-1);
      o.mpHasDlg = rows.length > 0;
    }
    o.mpAdvOpt = !!s.mpAdvOpt;
    o.mpAdvOptBg = s.mpAdvOpt ? '#30d158' : '#3a3a3f';
    o.mpAdvOptX = s.mpAdvOpt ? 20 : 2;
    o.mpToggleAdvOpt = () =>
      this.setState((t) =>
        t.mpAdvOpt ? { mpAdvOpt: false, mpDelay: 0, mpDelayTxt: null } : { mpAdvOpt: true },
      );
    o.mpSafelyOff = done;
    /* only once something was minted */ o.mpSafelyLabel = 'Connect Safely';
    o.mpSafelyOp = 1;
    o.mpConnectSafely = () => {
      const w = L && L.wallet,
        name = L && L.minter && L.minter.name;
      if (w)
        window.open(
          'https://link.safely.app/import-flame-devnet-wallet#testseed=' +
            w.mnemonic.trim().split(/\s+/).join('+') +
            (name ? '&walletName=' + encodeURIComponent(name) : ''),
          '_blank',
          'noopener',
        );
    }; // the demo (testnet) recovery phrase rides in the URL fragment, which never reaches a server; the button stays as is, since a page cannot tell whether Safely imported the wallet
    o.mpNameKey = (e) => {
      if (e.key === 'Enter') e.target.blur();
    };
    o.mpNoYouCta = !yOn && s.mpPhase === 'draft' && mine.length === 0;
    {
      const next = mine.filter((k) => k.a > now).sort((p, q) => p.a - q.a)[0];
      o.mpHasSched = !yOn && !!next;
      o.mpSchedIn = next ? ago(next.a - now) : '';
    }
    o.mpToastOn = !!s.mpToast;
    o.mpToastT1 = s.mpToastT1;
    o.mpHideToast = () => this.mpToastOut();
    o.mpToastAnim =
      s.mpToast === 'out'
        ? 'mpTipOut 220ms ease-in both'
        : 'mpToastIn 380ms cubic-bezier(0.2,0.9,0.3,1.2) both';
    o.mpResetAskOn = !!s.mpResetAsk;
    o.mpAskReset = () => this.setState({ mpResetAsk: true });
    o.mpResetCancel = () => this.setState({ mpResetAsk: false });
    // The last row of a list sits outside the group its sticky header belongs to, so the header lets go one row early.
    o.mpCMainIn = contribs.slice(0, -1);
    o.mpCMainOut = contribs.slice(-1);
    o.mpMinterCardsHead = minterCards.slice(0, -1);
    o.mpMinterCardsTail = minterCards.slice(-1);
    o.mpLiveChart = s.mpTab === 'live' ? this.mpLiveChart(c) : null;
    o.mpLiveLegend = [
      ...(c.cs.some((k) => k.who === 'you')
        ? [{ name: 'You', color: '#ff3b1f', ring: '#ff9a80' }]
        : []),
      { name: 'Others', color: '#ffc101', ring: '#ffc101' },
    ];
    const byName = s.mpPSort === 'name',
      pDir = s.mpPDir,
      arrow = pDir > 0 ? ' ↑' : ' ↓'; // tapping a column header sorts by it, tapping it again flips the order
    o.mpPNameLbl = 'Choose your minter' + (byName ? arrow : '');
    o.mpPFeeLbl = 'Commission' + (byName ? '' : arrow);
    o.mpSortName = () =>
      this.setState((t) =>
        t.mpPSort === 'name' ? { mpPDir: -t.mpPDir } : { mpPSort: 'name', mpPDir: 1 },
      );
    o.mpSortFee = () =>
      this.setState((t) =>
        t.mpPSort === 'fee' ? { mpPDir: -t.mpPDir } : { mpPSort: 'fee', mpPDir: 1 },
      );
    o.mpProviders = c.pids
      .slice()
      .sort((p, q) => {
        const a = this.mpM(p),
          b = this.mpM(q);
        return (
          pDir * (byName ? a.name.localeCompare(b.name) : a.fee - b.fee) ||
          a.name.localeCompare(b.name)
        );
      })
      .map((id) => {
        const m = this.mpM(id),
          on = id === c.selId;
        return {
          op: m.active ? 1 : 0.6,
          name: m.name,
          fee: m.fee,
          ring: on ? 'none' : 'inset 0 0 0 1.5px rgba(255,255,255,0.4)',
          dot: on ? '#0a84ff' : 'transparent',
          check: on ? 1 : 0,
          pick: () => this.setState({ mpMinter: id }),
        };
      });
    o.mpNoProviders = o.mpProviders.length === 0;
    o.mpHasProviders = !o.mpNoProviders;
    o.mpProvMask =
      o.mpProviders.length > 2 ? 'linear-gradient(#000 calc(100% - 12px), transparent)' : 'none';
    o.mpRewardMain = ok ? Math.round(c.reward).toLocaleString('en-US') + ' FLM' : '—';
    o.mpRewardNote = ok ? 'avg. ' + Math.round(c.share) + '% of each block' : '';
    const tb = s.mpTime === 'blocks',
      on = '#3a3a3f',
      fgOn = '#ffffff',
      fgOff = 'rgba(255,255,255,0.7)';
    [
      ['Blocks', 'blocks'],
      ['Days', 'days'],
    ].forEach(([k, v]) => {
      o['mpSw' + k] = () => {
        try {
          localStorage.setItem('flame.mpTime', v);
        } catch {
          /* the choice still applies here */
        }
        this.setState({ mpTime: v, mpDTxt: null, mpDelayTxt: null });
      };
      o['mpSw' + k + 'Bg'] = s.mpTime === v ? on : 'transparent';
      o['mpSw' + k + 'Fg'] = s.mpTime === v ? fgOn : fgOff;
    });
    [
      ['Sat', 'sat'],
      ['Btc', 'btc'],
      ['Usd', 'usd'],
    ].forEach(([k, v]) => {
      o['mpSw' + k] = () => {
        try {
          localStorage.setItem('flame.mpUnit', v);
        } catch {
          /* the choice still applies here */
        }
        this.setState({ mpUnit: v, mpBTxt: null });
      };
      o['mpSw' + k + 'Bg'] = s.mpUnit === v ? on : 'transparent';
      o['mpSw' + k + 'Fg'] = s.mpUnit === v ? fgOn : fgOff;
    });
    const dRound = Math.round(s.mpDays * 10) / 10;
    o.mpDaysVal = s.mpDTxt ?? (tb ? num(Math.round(s.mpDays * DAY)) : String(dRound));
    o.mpOnDays = (e) => {
      const v = e.target.value,
        n = parseFloat(String(v).replace(/[,\s]/g, ''));
      const d = isFinite(n) ? (tb ? n / DAY : n) : s.mpDays;
      this.setState({ mpDTxt: d > 31 ? null : v, mpDays: Math.max(1, Math.min(31, d)) });
    };
    o.mpBlurDays = () => this.setState({ mpDTxt: null });
    o.mpDaysW = Math.max(2, String(o.mpDaysVal).length + 0.6);
    o.mpDurUnit = tb ? 'blocks' : 'days';
    o.mpDelayVal =
      s.mpDelayTxt ??
      (tb ? num(Math.round(s.mpDelay * DAY)) : String(Math.round(s.mpDelay * 10) / 10));
    o.mpDelayW = Math.max(2, String(o.mpDelayVal).length + 0.6);
    o.mpOnDelay = (e) => {
      const v = e.target.value,
        n = parseFloat(String(v).replace(/[,\s]/g, ''));
      const d = isFinite(n) ? Math.max(0, tb ? n / DAY : n) : 0;
      this.setState({ mpDelayTxt: d > 31 ? null : v, mpDelay: Math.min(31, d) });
    };
    o.mpPctS = (Math.min(1, s.mpDelay / 31) * 100).toFixed(1);
    o.mpBlurDelay = () => this.setState({ mpDelayTxt: null });
    o.mpMcTitle = 'Mint rate, ' + (u === 'usd' ? '$' : u === 'btc' ? 'BTC' : 'sat') + '/FLM';
    o.mpMintChart = s.mpTab === 'mint' ? this.mpMintChart(c) : null;
    o.mpMintChartH = (this._mcH ?? 100) + 26;
    o.mpMintLegend = [
      ...(mine.length ? [{ name: 'New', color: 'rgba(255,59,31,0.4)', ring: '#ff9a80' }] : []),
      { name: 'You', color: '#ff3b1f', ring: '#ff9a80' },
      { name: 'Others', color: '#ffc101', ring: '#ffc101' },
    ];
    // Live engine: wallet, profile, submission.
    const wallet = L && L.wallet,
      ready = !!(wallet && L.connected()),
      busy = s.mpPhase === 'pending' || !!(L && L.submitting),
      pendingReq = L && !L.submitting && L.pending; // a request in flight is not yet a stuck one
    const commBps = Math.round(parseFloat(String(s.mpCommTxt ?? '').replace(',', '.')) * 100),
      commEdited = s.mpCommTxt != null && isFinite(commBps),
      saved = this.mpSavedComm();
    const commChange =
        !c.del &&
        s.mpAccept &&
        commEdited &&
        commBps !== Math.round((saved == null ? 2 : saved) * 100),
      commBad =
        !c.del &&
        s.mpAccept &&
        s.mpCommTxt != null &&
        !(isFinite(commBps) && commBps >= 0 && commBps <= 10000);
    o.mpIsPending = busy;
    o.mpNotPending = !busy;
    o.mpSubmitLabel = !wallet
      ? L && L.walletError
        ? 'Wallet unavailable'
        : 'Loading wallet…'
      : !ready
        ? 'Connecting…'
        : pendingReq
          ? 'Retry pending request'
          : 'Confirm burn #' + (c.cs.filter((k) => k.who === 'you').length + 1);
    const canSubmit = ready && !busy && (pendingReq || (ok && !commBad));
    o.mpSubmitOp = busy || canSubmit ? 1 : 0.4;
    o.mpHasSubmitMsg = !!(s.mpSubmitMsg || pendingReq || (L && L.walletError));
    o.mpSubmitMsg =
      s.mpSubmitMsg ||
      (pendingReq
        ? 'A previous request is unconfirmed. Retrying sends it again unchanged, so it is never accepted twice.'
        : L && L.walletError
          ? 'Wallet unavailable: ' + L.walletError
          : '');
    o.mpHasPendingReq = !!pendingReq && !busy;
    o.mpDiscard = () => {
      if (
        confirm(
          'Discard this pending request? It might already have been accepted. Discarding does not cancel it, and submitting again could create a second contribution.',
        )
      ) {
        L.discardPending();
        this.setState({ mpSubmitMsg: '' });
      }
    };
    const send = async () => {
      this.setState({ mpConfirm: false, mpPhase: 'pending', mpSubmitMsg: '' });
      const n = c.cs.filter((k) => k.who === 'you').length + 1; // same number as on the button
      try {
        if (!pendingReq && commChange) await L.saveCommission(commBps);
        const res = await L.submit(c.draft),
          later = c.delay > 0;
        clearTimeout(this._mpWinkT);
        this._mpWinkT = setTimeout(() => this.setState({ mpWinkId: null }), 8000);
        this.setState({
          mpWinkId: res.id,
          mpToast: true,
          mpPhase: 'draft',
          mpCommTxt: null,
          mpHi: 'you',
          mpToastT1:
            'Burn #' + n + (later ? ' starts in ' + this.mpDur(res.start_block - now) : ' started'),
        });
        clearTimeout(this._mpToastT);
        this._mpToastT = setTimeout(() => this.mpToastOut(), 4500);
      } catch (e) {
        this.setState({
          mpPhase: 'draft',
          mpSubmitMsg:
            (e.message || String(e)) + (L.pending ? '. The request is saved; retry safely.' : ''),
        });
      }
    };
    o.mpSubmit = () => {
      if (!canSubmit) return;
      if (pendingReq) send();
      else this.setState({ mpConfirm: true });
    };
    o.mpConfirmGo = () => {
      if (canSubmit) send();
    };
    // A saved request that got no answer is resent on its own once the server is back, at most every 5 s;
    // the server accepts each request id once, so this never burns twice.
    if (
      pendingReq &&
      canSubmit &&
      s.mpPhase === 'draft' &&
      Date.now() - (this._mpAutoRetry || 0) > 5000
    ) {
      this._mpAutoRetry = Date.now();
      setTimeout(send, 0);
    }
    o.mpCf = {
      ...o.mpCf,
      mode: c.del
        ? o.mpCf.mode
        : s.mpAccept
          ? 'Independent · accepts delegations' + (commChange ? ' at ' + commBps / 100 + '%' : '')
          : 'Independent',
    };
    const saveName = () => {
      const n = (s.mpName ?? '').trim();
      if (!L || s.mpName == null) return;
      if (!n) {
        this.setState({ mpName: null });
        return;
      }
      L.saveName(n).then(
        () => this.setState({ mpName: null }),
        (e) => {
          this.setState({ mpName: null, mpToast: true, mpToastT1: e.message || 'Name not saved' });
          clearTimeout(this._mpToastT);
          this._mpToastT = setTimeout(() => this.mpToastOut(), 4500);
        },
      );
    };
    o.mpName = s.mpName ?? ((L && L.minter && L.minter.name) || '');
    o.mpOnName = (e) =>
      this.setState({ mpName: L && L.cleanName ? L.cleanName(e.target.value) : e.target.value });
    o.mpNameBlur = () => saveName();
    o.mpResetGo = () => {
      L && L.resetWallet();
      this.setState({
        mpResetAsk: false,
        mpName: null,
        mpCommTxt: null,
        mpSubmitMsg: '',
        mpAccept: false,
        mpMode: 'minter',
      });
    };
    {
      const cur = L && L.wallet,
        saved = L && L.accounts ? L.accounts() : [],
        list = cur && !saved.some((w) => w.address === cur.address) ? [cur].concat(saved) : saved; // newest first; switching only moves the checkmark
      o.mpPrevWallets = list.map((w) => {
        const m = L.state?.minters?.find((a) => a.address === w.address),
          isCur = !!cur && w.address === cur.address,
          first = w === list[0];
        return {
          name: m ? m.name : isCur && L.minter ? L.minter.name : w.name || 'Empty account',
          sub:
            (m ? Math.round(Number(m.reward_sparks) / 1e8).toLocaleString('en-US') : '0') + ' FLM',
          isCur,
          bt: first ? 'none' : '0.5px solid rgba(255,255,255,0.2)',
          cur: isCur ? 'default' : 'pointer',
          pick: () => {
            if (isCur) return;
            L.switchWallet(w.address);
            this.setState({
              mpName: null,
              mpCommTxt: null,
              mpSubmitMsg: '',
              mpAccept: false,
              mpMode: 'minter',
            });
          },
        };
      });
    }
    Object.assign(o, this.mpModeTabs(o));
    o.mpMore = s.mpMore;
    o.mpScroller = this.scroller;
    o.mpOnScroll = () => this.scrollHints();
    o.mpRepeatTutorial = this.props.onRepeatTutorial;
    return o;
  }

  render() {
    return <PlaygroundScreen o={this.vals()} />;
  }
}
