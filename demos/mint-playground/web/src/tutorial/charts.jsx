// The stacked burn charts of the tutorial and the rewards they illustrate. Amounts are in units of
// 0.0001 BTC per block; the chart spans blocks 100–700 with "Now" at 250.
import React from 'react';

export const BURN_MAX = 25; // 0.0025 BTC per block at the right end of the slider
export const burnAt = (f) => Math.round(BURN_MAX * f * 10) / 10; // even track in 0.00001 BTC steps
export const burnPct = (b) => ((Math.max(0, b) / BURN_MAX) * 100).toFixed(2);

// "Anyone can burn bitcoins" and "Burn duration": the other minters (multiples of 144 blocks, each
// with its own palette colour; touching bars never match). ×0.88 so that any burn on those slides
// stays under 0.0050.
export const OTHERS = [
  [100, 244, 4.4, '#ffaf01'],
  [104, 248, 3.52, '#ffcd00'],
  [107, 251, 2.64, '#fef100'],
  [125, 269, 2.64, '#ffcd00'],
  [125, 269, 2.64, '#ffaf01'],
  [150, 294, 2.64, '#fef100'],
  [150, 294, 2.64, '#ffcd00'],
  [175, 319, 3.52, '#ffc101'],
  [186, 330, 1.76, '#ffaf01'],
  [252, 396, 1.76, '#ffd800'],
  [275, 419, 1.76, '#fef100'],
  [275, 419, 0.88, '#ffc101'],
  [296, 440, 2.64, '#ffd800'],
  [321, 465, 2.64, '#ffe400'],
  [337, 481, 0.88, '#ffc101'],
  [366, 510, 0.88, '#fef100'],
  [392, 536, 2.64, '#ffc101'],
  [392, 536, 0.88, '#ffe400'],
  [422, 566, 3.52, '#ffaf01'],
  [422, 566, 4.4, '#ffc101'],
  [441, 729, 3.52, '#ffcd00'],
  [441, 729, 0.88, '#ffd800'],
  [460, 748, 1.76, '#ffaf01'],
  [460, 604, 0.88, '#fef100'],
  [478, 622, 2.64, '#ffc101'],
  [478, 622, 1.76, '#ffaf01'],
  [507, 651, 5.28, '#ff9922'],
  [522, 810, 2.64, '#ffcd00'],
  [550, 838, 5.28, '#ffaf01'],
  [550, 694, 1.76, '#fef100'],
  [566, 710, 2.64, '#ff9922'],
  [566, 854, 2.64, '#ffe400'],
  [566, 710, 2.64, '#ffd800'],
  [566, 710, 1.76, '#ffcd00'],
  [591, 879, 2.64, '#fef100'],
  [605, 749, 1.76, '#ffe400'],
  [630, 774, 3.52, '#ffd800'],
  [630, 918, 2.64, '#ffe400'],
  [652, 796, 1.76, '#ff9922'],
  [652, 796, 3.52, '#fef100'],
];

/** Past the chart the network keeps burning as at its edge. */
export function reward6(from, to, per) {
  let rw = 0;
  for (let t = from; t < to && per > 0; t++) {
    const u = Math.min(t, 699);
    rw += (50 * per) / (per + OTHERS.reduce((x, r) => x + (r[0] <= u && r[1] > u ? r[2] : 0), 0));
  }
  return rw;
}

/** "Two ways to mint": an even network of others burning 0.25 BTC (2,500 units) a day. */
export function reward9(blocks, per) {
  return (blocks * 50 * per) / (per + 2500 / 144);
}

/**
 * Stacked bars over blocks 100–700, the last bar on top; bars past block 700 are cut at the edge.
 * Each bar is `[from, to, height, color]`; `top` is the height of the chart's top edge.
 */
export function StackChart({ bars, top }) {
  const P = bars.map((r) => [r[0], Math.min(r[1], 700), r[2], r[3]]);
  const X = (v) => 48 + ((v - 100) * 302) / 600;
  const Y = (v) => 100 - (v / top) * 92;
  const pts = [...new Set(P.flatMap((r) => [r[0], r[1]]))].sort((u, v) => u - v);
  const segs = [];
  for (let n = 0; n < pts.length - 1; n++) {
    const p = pts[n];
    const q = pts[n + 1];
    const mid = (p + q) / 2;
    let acc = 0;
    segs.push({
      p,
      q,
      band: P.map((r) => {
        if (!r[2] || r[0] > mid || r[1] <= mid) return null;
        const lo = acc;
        acc += r[2];
        return [Y(acc), Y(lo)];
      }),
    });
  }
  return (
    <svg
      width={350}
      height={100}
      viewBox="0 0 350 100"
      style={{ position: 'absolute', left: 0, top: 0, overflow: 'visible' }}
    >
      {P.map((r, m) => {
        const tp = [];
        const bt = [];
        segs.forEach((g) => {
          if (!g.band[m]) return;
          const [yt, yb] = g.band[m];
          tp.push([X(g.p), yt], [X(g.q), yt]);
          bt.unshift([X(g.q), yb], [X(g.p), yb]);
        });
        if (!tp.length) return null;
        const d =
          'M' +
          tp
            .concat(bt)
            .map(([a, c]) => a.toFixed(2) + ' ' + c.toFixed(2))
            .join('L') +
          'Z';
        return (
          <path key={m} d={d} fill={r[3]} stroke={r[3]} strokeWidth={1} strokeLinejoin="miter" />
        );
      })}
    </svg>
  );
}

const axisText = {
  position: 'absolute',
  fontSize: 12,
  lineHeight: '16px',
  color: '#ffffff',
  opacity: 0.64,
  fontVariantNumeric: 'tabular-nums',
};
const gridLine = { position: 'absolute', left: 48, width: 302, height: 1 };

/** The axes around a `StackChart`: "Total burned per block, BTC" up to 0.0050, blocks 100–700. */
export function BurnChart({ children }) {
  return (
    <div style={{ position: 'relative', width: 350, height: 144, marginTop: 34 }}>
      <div
        style={{
          position: 'absolute',
          left: 0,
          top: -22,
          fontSize: 12,
          lineHeight: '16px',
          fontWeight: 600,
          color: '#ffffff',
        }}
      >
        Total burned per block, BTC
      </div>
      <div style={{ ...axisText, left: 0, top: 92 }}>0</div>
      <div style={{ ...gridLine, top: 100, background: 'rgba(255,255,255,0.4)' }} />
      <div style={{ ...axisText, left: 0, top: 46 }}>0.0025</div>
      <div style={{ ...gridLine, top: 54, background: 'rgba(255,255,255,0.12)' }} />
      <div style={{ ...axisText, left: 0, top: 0 }}>0.0050</div>
      <div style={{ ...gridLine, top: 8, background: 'rgba(255,255,255,0.12)' }} />
      {children}
      <div
        style={{
          position: 'absolute',
          zIndex: 2,
          left: 123.5,
          top: 0,
          height: 120,
          width: 1.5,
          background: 'rgba(255,255,255,0.5)',
        }}
      />
      <div
        style={{
          position: 'absolute',
          left: 93.5,
          width: 60,
          top: 128,
          textAlign: 'center',
          fontSize: 12,
          lineHeight: '16px',
          fontWeight: 600,
          color: '#ffffff',
        }}
      >
        Now
      </div>
      {[100, 200, 300, 400, 500, 600, 700].map((v) => {
        const x = 48 + ((v - 100) * 302) / 600;
        return (
          <React.Fragment key={v}>
            <div
              style={{
                position: 'absolute',
                left: x,
                top: 100,
                height: 5,
                borderLeft: '1px solid rgba(255,255,255,0.4)',
              }}
            />
            <div style={{ ...axisText, left: x - 30, width: 60, top: 108, textAlign: 'center' }}>
              {v}
            </div>
          </React.Fragment>
        );
      })}
      <div
        style={{
          position: 'absolute',
          right: 0,
          top: 128,
          textAlign: 'right',
          fontSize: 12,
          lineHeight: '16px',
          fontWeight: 600,
          color: '#ffffff',
        }}
      >
        Block
      </div>
    </div>
  );
}
