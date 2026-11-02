import { useState } from 'react';
import { BurnChart, OTHERS, StackChart, burnAt, burnPct, reward6, reward9 } from './charts.jsx';
import { Track, useTrack } from './Track.jsx';

/** The design's `textOpacity` property: body text is slightly dimmed. */
export const TEXT_OPACITY = 0.64;
const NOW = 250;
const DAY = 144;
const number = (n) => n.toLocaleString('en-US');

const SCREEN = {
  position: 'absolute',
  left: '50%',
  top: 0,
  width: 390,
  height: 'var(--lh)',
  marginLeft: -195,
  transform: 'scale(var(--s))',
  transformOrigin: '50% 0',
  overflow: 'visible',
  background: 'transparent',
  display: 'flex',
  flexDirection: 'column',
};

export function Screen({ label, children }) {
  return (
    <div data-screen-label={label} style={SCREEN}>
      <div style={{ height: 'var(--st)', flexShrink: 0 }} />
      <div
        style={{
          position: 'relative',
          flex: 1,
          display: 'flex',
          flexDirection: 'column',
          padding: '24px 20px 16px',
          minHeight: 0,
        }}
      >
        {children}
      </div>
      <div style={{ height: 'var(--sb)', flexShrink: 0 }} />
    </div>
  );
}

const Spacer = () => <div style={{ flex: 1 }} />;

function Title({ children }) {
  return (
    <h1
      style={{
        margin: 0,
        fontWeight: 600,
        fontSize: 28,
        lineHeight: '34px',
        letterSpacing: '-0.02em',
        color: '#ffffff',
        textWrap: 'wrap',
      }}
    >
      {children}
    </h1>
  );
}

function Heading({ top, children }) {
  return (
    <h2
      style={{
        margin: `${top}px 0 0`,
        fontWeight: 600,
        fontSize: 20,
        lineHeight: '26px',
        letterSpacing: '-0.015em',
        color: '#ffffff',
      }}
    >
      {children}
    </h2>
  );
}

function Para({ top = 16, children }) {
  return (
    <p
      style={{
        margin: `${top}px 0 0`,
        fontSize: 18,
        lineHeight: '25px',
        color: '#ffffff',
        letterSpacing: '-0.01em',
        opacity: TEXT_OPACITY,
        textWrap: 'wrap',
      }}
    >
      {children}
    </p>
  );
}

const BUTTON = {
  height: 48,
  border: 'none',
  borderRadius: 24,
  font: 'inherit',
  fontSize: 17,
  fontWeight: 600,
  letterSpacing: '-0.41px',
  cursor: 'pointer',
};

function Button({ primary, wide, onClick, children }) {
  return (
    <button
      className="press"
      onClick={onClick}
      style={{
        ...BUTTON,
        padding: wide ? '0 16px' : '0 24px',
        background: primary ? '#0a84ff' : '#1a1a1d',
        color: primary ? '#ffffff' : '#ededed',
      }}
    >
      {children}
    </button>
  );
}

const DOTS = 8;
function Dots({ active }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
      {Array.from({ length: DOTS }, (_, i) => (
        <div
          key={i}
          style={{
            width: i === active ? 14 : 5,
            height: 5,
            borderRadius: 2.5,
            background: i === active ? '#ffffff' : 'rgba(255,255,255,0.28)',
          }}
        />
      ))}
    </div>
  );
}

const rowStyle = (hidden) => (hidden ? { visibility: 'hidden' } : {});

export function IntroRow({ className, hidden, onStart }) {
  return (
    <div
      className={className}
      style={{
        display: 'flex',
        flexDirection: 'column',
        gap: 16,
        flexShrink: 0,
        ...rowStyle(hidden),
      }}
    >
      <Button primary wide onClick={onStart}>
        Start Tutorial
      </Button>
      <p
        style={{
          margin: 0,
          textAlign: 'center',
          fontSize: 15,
          lineHeight: '20px',
          color: '#ffffff',
          opacity: 0.64,
        }}
      >
        This will take 3 minutes
      </p>
    </div>
  );
}

export function StepRow({ className, hidden, active, onBack, onNext }) {
  return (
    <div
      className={className}
      style={{
        display: 'flex',
        justifyContent: 'space-between',
        alignItems: 'center',
        flexShrink: 0,
        ...rowStyle(hidden),
      }}
    >
      <Button onClick={onBack}>Back</Button>
      <Dots active={active} />
      <Button primary onClick={onNext}>
        Next
      </Button>
    </div>
  );
}

export function FinishRow({ className, hidden, onBack, onStart }) {
  return (
    <div
      className={className}
      style={{
        display: 'grid',
        gridTemplateColumns: '1fr 1fr',
        columnGap: 12,
        flexShrink: 0,
        ...rowStyle(hidden),
      }}
    >
      <Button wide onClick={onBack}>
        Back
      </Button>
      <Button primary wide onClick={onStart}>
        Start Demo
      </Button>
    </div>
  );
}

// --- Diagram pieces ------------------------------------------------------------------------------

const Tick = ({ left, top, height }) => (
  <div
    style={{
      position: 'absolute',
      left,
      top,
      width: 1,
      height,
      background: 'rgba(255,255,255,0.5)',
    }}
  />
);
const Caption = ({ left, top, width = 132.1, children }) => (
  <div style={{ position: 'absolute', left, width, top, textAlign: 'center', fontWeight: 600 }}>
    {children}
  </div>
);
const Arrow = ({ left, top }) => (
  <svg
    width="37"
    height="16"
    viewBox="0 0 37 16"
    fill="none"
    stroke="rgba(255,255,255,0.7)"
    strokeWidth="1.2"
    strokeLinecap="round"
    strokeLinejoin="round"
    style={{ position: 'absolute', left, top }}
  >
    <path d="M2 8h32" />
    <path d="M30.5 4.5 34 8l-3.5 3.5" />
  </svg>
);
const Person = ({ size }) => (
  <svg width={size} height={size} viewBox="0 0 512 512">
    <circle cx="256" cy="128" r="115" fill="#ffffff" />
    <path
      d="M40 466C40 352 132 268 236 268H276C380 268 472 352 472 466V478C472 489 463 498 452 498H60C49 498 40 489 40 478Z"
      fill="#ffffff"
    />
  </svg>
);
const Avatar = ({ left, top, size, name }) => (
  <div
    style={{
      position: 'absolute',
      left,
      top,
      width: 80,
      display: 'flex',
      flexDirection: 'column',
      alignItems: 'center',
      gap: 6,
    }}
  >
    <Person size={size} />
    <span style={{ fontSize: 13, lineHeight: '16px', fontWeight: 600, color: '#ffffff' }}>
      {name}
    </span>
  </div>
);
const Chip = ({ children }) => (
  <span
    style={{
      padding: '7px 14px',
      borderRadius: 18,
      background: '#1a1a1d',
      boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.16)',
      fontSize: 15,
      lineHeight: '20px',
      fontWeight: 500,
      color: '#ffffff',
      whiteSpace: 'nowrap',
    }}
  >
    {children}
  </span>
);

const CARD = { background: '#1a1a1d', borderRadius: 20, display: 'flex', flexDirection: 'column' };
const label = { fontSize: 15, lineHeight: '20px', color: '#ffffff', opacity: 0.64 };
const value = {
  fontSize: 17,
  lineHeight: '22px',
  fontWeight: 600,
  color: '#ffffff',
  fontVariantNumeric: 'tabular-nums',
  whiteSpace: 'nowrap',
};
const Row = ({ children }) => (
  <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline' }}>
    {children}
  </div>
);

function Reward({ flm, pct }) {
  return (
    <>
      <div style={{ height: 1, background: 'rgba(255,255,255,0.12)', margin: '6px 0 12px' }} />
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'baseline',
          gap: 12,
          paddingBottom: 8,
        }}
      >
        <span style={label}>Your estimated reward</span>
        <span style={{ ...value, color: '#ff3b1f' }}>
          {flm} FLM <span style={{ opacity: 0.8 }}>({pct}%)</span>
        </span>
      </div>
    </>
  );
}

// --- The screens -----------------------------------------------------------------------------------

export function Intro({ onStart }) {
  return (
    <Screen label="00 Intro">
      <div
        style={{
          flex: 1,
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          textAlign: 'center',
        }}
      >
        <h1
          style={{
            margin: 0,
            fontWeight: 600,
            fontSize: 40,
            lineHeight: '46px',
            letterSpacing: '-0.025em',
            color: '#ffffff',
            textWrap: 'balance',
          }}
        >
          Introduction to Flame
        </h1>
      </div>
      <IntroRow hidden onStart={onStart} />
    </Screen>
  );
}

const barRow = { position: 'absolute', left: 0, top: 0, width: 350, display: 'flex' };
const bar = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  gap: 4,
  padding: '0 7px',
  boxSizing: 'border-box',
  whiteSpace: 'nowrap',
};

export function WhyFlame() {
  return (
    <Screen label="01 Why Flame">
      <Title>Why Flame</Title>
      <Para>
        The crypto economy is fragmented: Bitcoin is the trusted asset, but all the features are in
        altcoins.
      </Para>
      <div
        style={{
          position: 'relative',
          zIndex: 3,
          width: 350,
          height: 76,
          marginTop: 32,
          fontSize: 12,
          lineHeight: '16px',
          color: '#ffffff',
        }}
      >
        <div style={{ ...barRow, height: 32 }}>
          <div
            style={{ ...bar, width: 239.9, flexShrink: 0, background: '#ffc101', color: '#000000' }}
          >
            <span style={{ fontWeight: 600 }}>Bitcoin</span>
            <span style={{ opacity: 0.8, fontVariantNumeric: 'tabular-nums' }}>$1.3T</span>
          </div>
          <div
            style={{
              ...bar,
              flex: 1,
              minWidth: 0,
              background: '#5a5a61',
              color: '#ffffff',
              marginLeft: -1,
            }}
          >
            <span style={{ fontWeight: 600 }}>Altcoins</span>
            <span style={{ opacity: 0.8, fontVariantNumeric: 'tabular-nums' }}>$0.6T</span>
          </div>
        </div>
        <Tick left={119.4} top={36} height={20} />
        <Caption left={53.9} top={60}>
          Store of value
        </Caption>
        <Tick left={295} top={36} height={20} />
        <Caption left={229.5} top={60}>
          Functionality
        </Caption>
      </div>
      <Para top={24}>Flame extends Bitcoin with private payments, speed and smart contracts.</Para>
      <div
        style={{
          position: 'relative',
          zIndex: 3,
          width: 350,
          height: 108,
          marginTop: 32,
          fontSize: 12,
          lineHeight: '16px',
          color: '#ffffff',
        }}
      >
        <div style={{ ...barRow, height: 64 }}>
          <div
            style={{
              width: 204.7,
              flexShrink: 0,
              background: '#ffc101',
              color: '#000000',
              display: 'flex',
              alignItems: 'flex-end',
              justifyContent: 'space-between',
              gap: 6,
              padding: '0 10px 10px',
              boxSizing: 'border-box',
            }}
          >
            <span style={{ fontSize: 13, lineHeight: '16px', fontWeight: 600 }}>Bitcoin</span>
          </div>
          <div
            style={{
              position: 'relative',
              overflow: 'hidden',
              width: 129.9,
              flexShrink: 0,
              background: '#ff3b1f',
              marginLeft: -1,
              color: '#ffffff',
              display: 'flex',
              alignItems: 'flex-end',
              justifyContent: 'space-between',
              gap: 6,
              padding: '0 10px 10px',
              boxSizing: 'border-box',
            }}
          >
            <span
              style={{
                position: 'relative',
                zIndex: 1,
                fontSize: 13,
                lineHeight: '16px',
                fontWeight: 600,
                color: '#ffffff',
              }}
            >
              Flame
            </span>
          </div>
          <div style={{ flex: 1, minWidth: 0, marginLeft: -1, background: '#5a5a61' }} />
        </div>
        <Tick left={101.8} top={68} height={20} />
        <Caption left={36.3} top={92}>
          Store of value
        </Caption>
        <Tick left={269.1} top={68} height={20} />
        <Caption left={203.6} top={92}>
          Functionality
        </Caption>
      </div>
      <Spacer />
      <StepRow hidden active={0} />
    </Screen>
  );
}

const box = {
  position: 'absolute',
  top: 28,
  width: 84,
  height: 44,
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
  fontSize: 13,
  lineHeight: '16px',
  fontWeight: 600,
  color: '#ffffff',
};

export function ProofOfBurn() {
  return (
    <Screen label="02 Proof of burn">
      <Title>Proof of burn</Title>
      <Para>Flame stands on Bitcoin: flames are created by destroying bitcoins.</Para>
      <div style={{ position: 'relative', width: 350, height: 72, marginTop: 32 }}>
        <div
          style={{
            position: 'absolute',
            left: 108,
            top: 20,
            width: 1,
            height: 26,
            background: 'rgba(255,255,255,0.5)',
          }}
        />
        <div
          style={{
            position: 'absolute',
            left: 241,
            top: 20,
            width: 1,
            height: 26,
            background: 'rgba(255,255,255,0.5)',
          }}
        />
        <div
          style={{
            position: 'absolute',
            left: 58.5,
            width: 100,
            top: 0,
            textAlign: 'center',
            fontSize: 12,
            lineHeight: '16px',
            fontWeight: 600,
            color: '#ffffff',
          }}
        >
          Proof of work
        </div>
        <div
          style={{
            position: 'absolute',
            left: 191.5,
            width: 100,
            top: 0,
            textAlign: 'center',
            fontSize: 12,
            lineHeight: '16px',
            fontWeight: 600,
            color: '#ffffff',
          }}
        >
          Proof of burn
        </div>
        <div style={{ ...box, left: 0, background: '#5a5a61' }}>Energy</div>
        <Arrow left={90} top={42} />
        <div style={{ ...box, left: 133, background: '#ffc101', color: '#000000' }}>Bitcoins</div>
        <Arrow left={223} top={42} />
        <div style={{ ...box, left: 266, overflow: 'hidden', background: '#ff3b1f' }}>
          <span style={{ position: 'relative', zIndex: 1 }}>Flames</span>
        </div>
      </div>
      <Para top={32}>
        Burned bitcoin can’t be recovered, so rewriting Flame’s history costs real money.
      </Para>
      <Para top={12}>21M total supply, 50 flames per block, halving every 4 years.</Para>
      <Spacer />
      <StepRow hidden active={1} />
    </Screen>
  );
}

export function Privacy() {
  return (
    <Screen label="03 Confidential transfers">
      <Title>Confidential transfers</Title>
      <Para>Only the sender and receiver see what was sent and how much.</Para>
      <div style={{ position: 'relative', width: 350, height: 128, marginTop: 32 }}>
        <Avatar left={-12} top={0} size={32} name="Alice" />
        <Avatar left={282} top={0} size={32} name="Bob" />
        <svg
          width="350"
          height="32"
          viewBox="0 0 350 32"
          fill="none"
          stroke="rgba(255,255,255,0.7)"
          strokeWidth="1.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          style={{ position: 'absolute', left: 0, top: 0 }}
        >
          <path d="M58 16H292" />
          <path d="M288.5 12.5 292 16l-3.5 3.5" />
        </svg>
        <div
          style={{
            position: 'absolute',
            left: 143,
            top: 2,
            width: 64,
            height: 28,
            background: '#2c2c31',
            boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.7)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            fontSize: 12,
            lineHeight: '16px',
            fontWeight: 600,
            letterSpacing: '0.08em',
            color: '#ffffff',
          }}
        >
          xxx ??
        </div>
        <div
          style={{
            position: 'absolute',
            left: 174.5,
            top: 34,
            width: 1,
            height: 42,
            background:
              'repeating-linear-gradient(to bottom,rgba(255,255,255,0.5) 0 3px,transparent 3px 6px)',
          }}
        />
        <Avatar left={135} top={80} size={28} name="Carol" />
      </div>
      <Para top={24}>
        The transaction graph remains transparent for smart contracts and scalability.
      </Para>
      <Spacer />
      <StepRow hidden active={2} />
    </Screen>
  );
}

const Line = ({ left, right }) => (
  <div
    style={{
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'space-between',
      gap: 12,
      height: 48,
      color: '#ffffff',
    }}
  >
    <span style={{ fontSize: 15, lineHeight: '20px', opacity: 0.72 }}>{left}</span>
    <span
      style={{
        fontSize: 15,
        lineHeight: '20px',
        fontWeight: 600,
        whiteSpace: 'nowrap',
        fontVariantNumeric: 'tabular-nums',
      }}
    >
      {right}
    </span>
  </div>
);
const Divider = () => <div style={{ height: 1, background: 'rgba(255,255,255,0.16)' }} />;

export function Speed() {
  return (
    <Screen label="04 Fast payments">
      <Title>Fast payments</Title>
      <Para>Small payments confirm in seconds. Large ones need more time.</Para>
      <div
        style={{
          marginTop: 32,
          display: 'flex',
          flexDirection: 'column',
          background: '#1a1a1d',
          borderRadius: 16,
          padding: '4px 16px',
        }}
      >
        <Line left="Flame i-block" right="2–10 sec" />
        <Divider />
        <Line left="Bitcoin block" right="10–20 min" />
        <Divider />
        <Line left="6 blocks" right="1 hour" />
      </div>
      <Para top={24}>Like in Bitcoin, the recipient decides when a transaction is final.</Para>
      <Spacer />
      <StepRow hidden active={3} />
    </Screen>
  );
}

export function FlameVM() {
  return (
    <Screen label="05 FlameVM">
      <Title>FlameVM</Title>
      <Para>A Turing-complete VM for encrypted and unencrypted data, with linear types.</Para>
      <Para>You can build anything:</Para>
      <div style={{ marginTop: 24, display: 'flex', flexWrap: 'wrap', gap: 8 }}>
        {[
          'escrow',
          'multisig',
          'payment channel',
          'stablecoin',
          'custom token',
          'DEX',
          'vesting',
          'salary system',
          'dead man’s switch',
          'treasury',
        ].map((name) => (
          <Chip key={name}>{name}</Chip>
        ))}
        <span
          style={{
            padding: '7px 4px',
            fontSize: 15,
            lineHeight: '20px',
            fontWeight: 500,
            color: '#ffffff',
            opacity: 0.7,
          }}
        >
          …
        </span>
      </div>
      <Spacer />
      <StepRow hidden active={4} />
    </Screen>
  );
}

/** "Anyone can burn bitcoins": burn amount per block, 144 blocks from Now. */
export function BurnAmount({ burn, setBurn }) {
  const track = useTrack((f) => setBurn(burnAt(f)));
  const rw = reward6(NOW, NOW + DAY, burn);
  return (
    <Screen label="06 Anyone can burn bitcoins">
      <Title>Anyone can burn bitcoins</Title>
      <Para>
        Every bitcoin user can be a minter. All users compete for a fixed supply of flames.
      </Para>
      <BurnChart>
        <StackChart bars={OTHERS.concat([[NOW, NOW + DAY, burn, '#ff3b1f']])} top={50} />
      </BurnChart>
      <div style={{ ...CARD, marginTop: 20, padding: '14px 16px 6px' }}>
        <Row>
          <span style={label}>Burn amount</span>
          <span style={value}>{(burn / 10000).toFixed(5)} BTC</span>
        </Row>
        <Track track={track} pct={burnPct(burn)} height={44} top={20} />
        <Reward flm={number(Math.round(rw))} pct={Math.round((rw / (50 * DAY)) * 100)} />
      </div>
      <Para top={20}>
        Every Bitcoin block, each minter gets a share of flames proportional to the bitcoins they
        burned.
      </Para>
      <Spacer />
      <StepRow hidden active={5} />
    </Screen>
  );
}

/** "Burn duration": the amount chosen before (144 blocks × burn per block) spread over 144–1008 blocks. */
export function BurnDuration({ burn, days, setDays }) {
  const track = useTrack((f) => setDays(1 + Math.round(f * 6)));
  const per = burn / days;
  const end = NOW + DAY * days;
  const rw = reward6(NOW, end, per);
  return (
    <Screen label="07 Burn duration">
      <Title>Burn duration</Title>
      <Para>Users burn continuously, block after block. Spread your burn over more blocks.</Para>
      <BurnChart>
        <StackChart bars={OTHERS.concat([[NOW, end, per, '#ff3b1f']])} top={50} />
      </BurnChart>
      <div style={{ ...CARD, marginTop: 20, padding: '14px 16px 6px' }}>
        <Row>
          <span style={label}>Duration</span>
          <span style={value}>{number(DAY * days)} blocks</span>
        </Row>
        <Track track={track} pct={(((days - 1) / 6) * 100).toFixed(1)} height={44} top={20} />
        <Reward flm={number(Math.round(rw))} pct={Math.round((rw / (50 * DAY * days)) * 100)} />
      </div>
      <Para top={20}>
        For network security, every burn lasts at least 144 blocks — about one day.
      </Para>
      <Spacer />
      <StepRow hidden active={6} />
    </Screen>
  );
}

const Stat = ({ name, children, red }) => (
  <div style={{ display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0 }}>
    <span style={{ fontSize: 13, lineHeight: '16px', color: 'rgba(255,255,255,0.64)' }}>
      {name}
    </span>
    <span
      style={{
        fontSize: 17,
        lineHeight: '22px',
        fontWeight: 600,
        fontVariantNumeric: 'tabular-nums',
        whiteSpace: 'nowrap',
        color: red ? '#ff3b1f' : '#ffffff',
      }}
    >
      {children}
    </span>
  </div>
);
const Stats = ({ children }) => (
  <div
    style={{
      marginTop: 12,
      background: '#1a1a1d',
      borderRadius: 16,
      padding: '10px 16px',
      display: 'grid',
      gridTemplateColumns: '1fr 1fr',
      gap: 12,
    }}
  >
    {children}
  </div>
);

/** "Two ways to mint": its own budget and duration, what each way costs and earns. */
export function TwoWays({ budget, setBudget, days, setDays }) {
  // Its own 0.05–1 BTC budget in 0.01 BTC steps, so a month's mining fees (0.0432 BTC) always fit.
  const budgetTrack = useTrack((f) => setBudget((5 + Math.round(f * 95)) / 100));
  const daysTrack = useTrack((f) => setDays(1 + Math.round(f * 29)));
  const b = (budget * 10000) / DAY;
  const fees = (DAY * days * 1000) / 1e8;
  const flm = (v) =>
    (v < 10 ? v.toFixed(2) : v < 100 ? v.toFixed(1) : number(Math.round(v))) + ' FLM';
  // The budget covers mining fees too (1,000 sat = 0.1 units per block), so independent burns less;
  // delegation burns it all and its minter keeps 10%.
  const rwA = reward9(DAY * days, Math.max(0, b - 0.1 * days) / days);
  const rwB = reward9(DAY * days, b / days);
  const comm = rwB * 0.1;
  return (
    <Screen label="09 Two ways to mint">
      <Title>Two ways to mint</Title>
      <Heading top={20}>A. Independent</Heading>
      <Para top={8}>
        Stay online and pay a Bitcoin mining fee for every block — about $100–150 per day.
      </Para>
      <Stats>
        <Stat name="Mining fees">{+fees.toFixed(4)} BTC</Stat>
        <Stat name="Earned" red>
          {flm(rwA)}
        </Stat>
      </Stats>
      <Heading top={24}>B. Delegation</Heading>
      <Para top={8}>
        Choose a reliable minter, burn bitcoins once a day or week, and stay offline. No mining fees
        — the minter keeps a share of the flames you earn.
      </Para>
      <Stats>
        <Stat name="Commission 10%">{flm(comm)}</Stat>
        <Stat name="Earned" red>
          {flm(rwB - comm)}
        </Stat>
      </Stats>
      <div style={{ ...CARD, marginTop: 20, padding: '12px 16px 4px' }}>
        <Row>
          <span style={label}>Budget</span>
          <span style={value}>{+budget.toFixed(2)} BTC</span>
        </Row>
        <Track
          track={budgetTrack}
          pct={(((budget - 0.05) / 0.95) * 100).toFixed(2)}
          height={40}
          top={18}
        />
        <Row>
          <span style={label}>Duration</span>
          <span style={value}>
            {days} {days === 1 ? 'day' : 'days'}
          </span>
        </Row>
        <Track track={daysTrack} pct={(((days - 1) / 29) * 100).toFixed(1)} height={40} top={18} />
      </div>
      <Spacer />
      <StepRow hidden active={7} />
    </Screen>
  );
}

const Step = ({ n, children }) => (
  <div style={{ display: 'flex', alignItems: 'flex-start', gap: 12 }}>
    <span
      style={{
        width: 24,
        height: 24,
        flexShrink: 0,
        borderRadius: 12,
        background: '#1a1a1d',
        boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.16)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        fontSize: 13,
        lineHeight: '16px',
        fontWeight: 600,
        color: '#ffffff',
      }}
    >
      {n}
    </span>
    <span
      style={{
        paddingTop: 1,
        fontSize: 17,
        lineHeight: '22px',
        color: '#ffffff',
        textWrap: 'wrap',
      }}
    >
      {children}
    </span>
  </div>
);

export function TryIt({ onBack, onStart }) {
  return (
    <Screen label="10 Try minting now">
      <div
        style={{
          flex: 1,
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          textAlign: 'center',
          gap: 16,
        }}
      >
        <h1
          style={{
            margin: 0,
            fontWeight: 600,
            fontSize: 40,
            lineHeight: '46px',
            letterSpacing: '-0.025em',
            color: '#ffffff',
            textWrap: 'balance',
          }}
        >
          Try minting now
        </h1>
        <p
          style={{
            margin: 0,
            fontSize: 18,
            lineHeight: '25px',
            color: '#ffffff',
            letterSpacing: '-0.01em',
            opacity: TEXT_OPACITY,
            textWrap: 'balance',
          }}
        >
          It’s a demo —
          <br />
          no real bitcoins, no real flames.
        </p>
        <div
          style={{
            marginTop: 16,
            alignSelf: 'stretch',
            display: 'flex',
            flexDirection: 'column',
            gap: 14,
            textAlign: 'left',
            background: '#1a1a1d',
            borderRadius: 16,
            padding: 16,
          }}
        >
          <Step n={1}>Set your budget and duration</Step>
          <Step n={2}>Mint independently or delegate</Step>
          <Step n={3}>Start accumulating flames</Step>
        </div>
        <p
          style={{
            margin: '4px 0 0',
            fontSize: 15,
            lineHeight: '20px',
            color: 'rgba(255,255,255,0.6)',
          }}
        >
          Read more on{' '}
          <a
            href="https://runflame.org"
            target="_blank"
            rel="noopener"
            style={{ color: '#0a84ff', textDecoration: 'none' }}
          >
            runflame.org
          </a>
        </p>
      </div>
      <FinishRow hidden onBack={onBack} onStart={onStart} />
    </Screen>
  );
}

/** The sliders of screens 6, 7 and 9 build on each other; this keeps them together. */
export function useTutorialSliders() {
  const [burn, setBurn] = useState(20);
  const [days, setDays] = useState(1);
  const [budget9, setBudget9] = useState(null);
  const [days9, setDays9] = useState(null);
  return {
    burn,
    setBurn,
    days,
    setDays,
    // "Two ways to mint" starts from the earlier choices until its own sliders are moved.
    budget: budget9 ?? Math.max(5, Math.round(burn * 1.44)) / 100,
    setBudget: setBudget9,
    days9: days9 ?? days,
    setDays9,
  };
}
