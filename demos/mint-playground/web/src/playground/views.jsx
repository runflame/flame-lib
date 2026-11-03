// The Mint Playground's screen: the tab bar, the four tabs, the pages pushed over them and the
// sheets. Everything shown comes from `o`, the values Playground.jsx derives.

const DIM = 'rgba(255,255,255,0.7)';
const HAIRLINE = '0.5px solid rgba(255,255,255,0.2)';
const CARD = { background: '#1a1a1d', borderRadius: 16, display: 'flex', flexDirection: 'column' };
const LIST = { ...CARD, padding: '0 16px 4px', overflow: 'clip' };
const SPLIT = { display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 12 };
const LABEL = { fontSize: 15, lineHeight: '20px', color: DIM };
const VALUE = {
  fontSize: 17,
  lineHeight: '22px',
  fontWeight: 600,
  fontVariantNumeric: 'tabular-nums',
  whiteSpace: 'nowrap',
};
const ELLIPSIS = { whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' };
const TITLE = { fontSize: 17, lineHeight: '21px', fontWeight: 600, ...ELLIPSIS };
const BADGE = {
  fontSize: 12,
  lineHeight: '14px',
  fontWeight: 600,
  padding: '1px 6px',
  borderRadius: 8,
  whiteSpace: 'nowrap',
};
const YOU = { ...BADGE, background: '#d1d1d6', color: '#1c1c1e' };
const ROLE = { ...BADGE, background: 'rgba(255,255,255,0.08)', color: 'rgba(255,255,255,0.75)' };
const COLUMN = { display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0 };
const UNIT = { marginLeft: 4, fontSize: 13, fontWeight: 500, color: DIM };
// A list row: full width of its card, separated by a hairline.
const ROW = {
  display: 'flex',
  flexDirection: 'column',
  gap: 6,
  minHeight: 60,
  padding: '10px 16px',
  margin: '0 -16px',
  boxSizing: 'border-box',
  borderTop: HAIRLINE,
  fontVariantNumeric: 'tabular-nums',
};
// A list's header, staying put while its rows scroll under it.
const STICKY = {
  display: 'grid',
  gap: 12,
  position: 'sticky',
  top: -1,
  zIndex: 2,
  margin: '0 -16px',
  padding: '15px 16px 10px',
  background: '#1a1a1d',
  fontSize: 13,
  lineHeight: '16px',
  color: DIM,
};
const INPUT = {
  textAlign: 'right',
  border: 'none',
  outline: 'none',
  background: 'transparent',
  color: '#ffffff',
  font: 'inherit',
  fontSize: 17,
  lineHeight: '22px',
  fontWeight: 600,
  padding: 0,
};
const FIELD = {
  display: 'flex',
  alignItems: 'baseline',
  padding: '2px 10px',
  borderRadius: 10,
  background: 'rgba(255,255,255,0.06)',
  cursor: 'text',
};
const BUTTON = {
  border: 'none',
  font: 'inherit',
  fontSize: 17,
  fontWeight: 600,
  cursor: 'pointer',
};
const PRIMARY = {
  ...BUTTON,
  height: 52,
  borderRadius: 26,
  background: '#0a84ff',
  color: '#ffffff',
};
const PILL = {
  flexShrink: 0,
  height: 32,
  padding: '0 14px',
  borderRadius: 16,
  background: '#0a84ff',
  color: '#ffffff',
  fontSize: 15,
  lineHeight: '32px',
  fontWeight: 600,
};
const TOAST = {
  display: 'flex',
  alignItems: 'center',
  height: 40,
  padding: '0 18px',
  borderRadius: 20,
  background: '#0a84ff',
  boxShadow: '0 6px 20px rgba(10,132,255,0.35),0 2px 6px rgba(0,0,0,0.3)',
  color: '#ffffff',
  fontSize: 15,
  lineHeight: '20px',
  fontWeight: 600,
  whiteSpace: 'nowrap',
};
const TOP = {
  position: 'absolute',
  left: 0,
  right: 0,
  top: 'calc(var(--st) + 10px)',
  display: 'flex',
  justifyContent: 'center',
  pointerEvents: 'none',
};

const Chevron = ({ color = 'currentColor' }) => (
  <svg
    width="8"
    height="14"
    viewBox="0 0 8 14"
    fill="none"
    stroke={color}
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    style={{ flexShrink: 0 }}
  >
    <path d="M1 1l6 6-6 6" />
  </svg>
);
const Check = ({ color, width }) => (
  <svg
    width="22"
    height="22"
    viewBox="0 0 24 24"
    fill="none"
    stroke={color}
    strokeWidth={width}
    strokeLinecap="round"
    strokeLinejoin="round"
    style={{ flexShrink: 0 }}
  >
    <path d="M5 12.5 10 17.5 19 7.5" />
  </svg>
);
const Dot = ({ color }) => (
  <span style={{ width: 8, height: 8, borderRadius: 2, background: color }} />
);

/** A tab's chart title with the colours of its bands. */
function Legend({ title, items }) {
  return (
    <div
      style={{
        marginTop: 0,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        gap: 8,
        fontSize: 12,
        lineHeight: '16px',
        color: '#ffffff',
      }}
    >
      <span style={{ fontWeight: 600, whiteSpace: 'nowrap' }}>{title}</span>
      <div style={{ display: 'flex', gap: 10, whiteSpace: 'nowrap' }}>
        {items.map((l, i) => (
          <span key={i} style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
            <span
              style={{
                width: 8,
                height: 8,
                borderRadius: 2,
                background: l.color,
                boxShadow: `inset 0 0 0 1px ${l.ring}`,
              }}
            />
            <span style={{ fontWeight: 600 }}>{l.name}</span>
          </span>
        ))}
      </div>
    </div>
  );
}

/** The Mint tab's sliders: a bar filled up to `pct` percent and the knob there. */
function Slider({ track, onDown, pct }) {
  return (
    <div
      ref={track}
      data-noswipe=""
      onPointerDown={onDown}
      style={{
        position: 'relative',
        height: 28,
        margin: '0 12px',
        touchAction: 'none',
        cursor: 'pointer',
      }}
    >
      <div
        style={{
          position: 'absolute',
          left: -12,
          right: -12,
          top: 12,
          height: 4,
          borderRadius: 2,
          background: 'rgba(255,255,255,0.16)',
        }}
      />
      <div
        style={{
          position: 'absolute',
          left: -12,
          top: 12,
          height: 4,
          borderRadius: 2,
          background: '#ff3b1f',
          width: `calc(${pct}% + 12px)`,
        }}
      />
      <div
        style={{
          position: 'absolute',
          top: 2,
          width: 24,
          height: 24,
          marginLeft: -12,
          borderRadius: 10,
          background: '#ffffff',
          boxShadow: '0 2px 8px rgba(0,0,0,0.5)',
          left: `${pct}%`,
        }}
      />
    </div>
  );
}

/** A typed amount with its unit beside it. */
function Amount({ value, onChange, onBlur, width, maxWidth, unit, placeholder }) {
  return (
    <div style={{ display: 'flex', alignItems: 'baseline', gap: 8 }}>
      <label style={FIELD}>
        <input
          value={value}
          onChange={onChange}
          onBlur={onBlur}
          placeholder={placeholder}
          inputMode="decimal"
          style={{ ...INPUT, width: `${width}ch`, maxWidth, fontVariantNumeric: 'tabular-nums' }}
        />
      </label>
      <span style={{ ...LABEL, color: '#ffffff', opacity: 0.7 }}>{unit}</span>
    </div>
  );
}

function Switch({ on, left }) {
  return (
    <span
      style={{
        position: 'relative',
        flexShrink: 0,
        width: 44,
        height: 26,
        borderRadius: 13,
        background: on,
        transition: 'background 160ms',
      }}
    >
      <span
        style={{
          position: 'absolute',
          top: 2,
          left,
          width: 22,
          height: 22,
          borderRadius: 11,
          background: '#ffffff',
          boxShadow: '0 2px 4px rgba(0,0,0,0.3)',
          transition: 'left 160ms',
        }}
      />
    </span>
  );
}

function NameField({ o, disabled, pointerEvents, width, padding }) {
  const { mpName, mpOnName, mpFocusEnd, mpNameBlur, mpNameKey } = o;
  return (
    <label style={{ ...FIELD, minWidth: 0, padding }}>
      <input
        value={mpName}
        onChange={mpOnName}
        disabled={disabled}
        onFocus={mpFocusEnd}
        onBlur={mpNameBlur}
        onKeyDown={mpNameKey}
        enterKeyHint="done"
        placeholder="Add a name"
        maxLength={64}
        autoComplete="off"
        spellCheck="false"
        style={{
          ...INPUT,
          pointerEvents,
          opacity: 1,
          WebkitTextFillColor: '#ffffff',
          width,
          maxWidth: '100%',
          textOverflow: 'ellipsis',
        }}
      />
    </label>
  );
}

// --- Tabs -------------------------------------------------------------------------------------------

function NetworkTab({ o }) {
  const {
    mpAvgRate,
    mpGoMe,
    mpGoMint,
    mpHasPl,
    mpHasSched,
    mpLiveChart,
    mpLiveLegend,
    mpMcTitle,
    mpMintChartH,
    mpNContribs,
    mpNMinters,
    mpNoStakes,
    mpNoStakesText,
    mpNoYouCta,
    mpOpenContribs,
    mpOpenMinters,
    mpPl,
    mpS1,
    mpS2,
    mpSchedIn,
  } = o;
  const line = (name, value, first) => (
    <div style={{ ...SPLIT, minHeight: 48, borderTop: first ? undefined : HAIRLINE }}>
      <span style={LABEL}>{name}</span>
      <span style={VALUE}>{value}</span>
    </div>
  );
  const link = (onClick, name, value) => (
    <div
      onClick={onClick}
      style={{
        ...CARD,
        ...SPLIT,
        marginTop: 12,
        padding: '0 16px',
        minHeight: 52,
        flexDirection: 'row',
        cursor: 'pointer',
      }}
    >
      <span style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <Dot color="#ff3b1f" />
        <span style={{ fontSize: 17, lineHeight: '22px' }}>{name}</span>
      </span>
      <span style={{ display: 'flex', alignItems: 'center', gap: 10, color: DIM }}>
        <span style={{ ...VALUE, color: '#ffffff' }}>{value}</span>
        <Chevron />
      </span>
    </div>
  );
  const count = (onClick, name, value, first) => (
    <div
      onClick={onClick}
      style={{
        ...SPLIT,
        minHeight: 48,
        cursor: 'pointer',
        borderTop: first ? undefined : HAIRLINE,
      }}
    >
      <span style={{ fontSize: 17, lineHeight: '22px' }}>{name}</span>
      <span style={{ display: 'flex', alignItems: 'center', gap: 10, color: DIM }}>
        <span style={{ fontSize: 17, lineHeight: '22px', fontVariantNumeric: 'tabular-nums' }}>
          {value}
        </span>
        <Chevron />
      </span>
    </div>
  );
  return (
    <div style={{ padding: '12px 20px 24px', display: 'flex', flexDirection: 'column' }}>
      <Legend title={mpMcTitle} items={mpLiveLegend} />
      <div style={{ position: 'relative', width: 350, height: mpMintChartH, marginTop: 26 }}>
        {mpLiveChart}
        {mpNoStakes ? (
          <div
            style={{
              position: 'absolute',
              left: 48,
              right: 0,
              top: 0,
              height: 100,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              fontSize: 13,
              lineHeight: '18px',
              color: '#ffffff',
              opacity: 0.7,
            }}
          >
            {mpNoStakesText}
          </div>
        ) : null}
      </div>
      <div style={{ height: 12 }} />
      <div style={{ ...CARD, padding: '0 16px' }}>
        {line('Mint rate', mpPl.mkt, true)}
        {line('Bitcoins burned', mpS2.value)}
        {line('Flames created', mpS1.value)}
      </div>
      {mpNoYouCta ? (
        <div
          onClick={mpGoMint}
          style={{
            ...CARD,
            ...SPLIT,
            marginTop: 12,
            padding: '12px 12px 12px 14px',
            boxShadow: 'inset 0 0 0 1.5px rgba(10,132,255,0.6)',
            flexDirection: 'row',
            cursor: 'pointer',
          }}
        >
          <div style={COLUMN}>
            <span style={{ fontSize: 15, lineHeight: '20px', fontWeight: 600 }}>
              You’re not minting yet
            </span>
          </div>
          <span style={PILL}>Mint</span>
        </div>
      ) : null}
      {mpHasSched ? link(mpGoMe, 'Your burn starts in', mpSchedIn) : null}
      {mpHasPl ? link(mpGoMe, 'Your avg mint cost', mpAvgRate) : null}
      <div style={{ height: 24 }} />
      <div style={{ ...CARD, padding: '0 16px' }}>
        {count(mpOpenMinters, 'Minters', mpNMinters, true)}
        {count(mpOpenContribs, 'Burns', mpNContribs)}
      </div>
    </div>
  );
}

function MintTab({ o }) {
  const {
    mpAccBg,
    mpAccX2,
    mpAdvOpt,
    mpBlurBudget,
    mpBlurComm,
    mpBlurDays,
    mpBlurDelay,
    mpBudgetRing,
    mpBudgetVal,
    mpBudgetW,
    mpCommVal,
    mpCommW,
    mpDaysVal,
    mpDaysW,
    mpDelayVal,
    mpDelayW,
    mpDiscard,
    mpDownB,
    mpDownD,
    mpDownS,
    mpDurUnit,
    mpErr,
    mpFocusEnd,
    mpHasErr,
    mpHasPendingReq,
    mpHasProviders,
    mpHasSubmitMsg,
    mpIsDel,
    mpIsMinter,
    mpIsPending,
    mpMcTitle,
    mpMintChart,
    mpMintChartH,
    mpMintLegend,
    mpNoProvText,
    mpNoProviders,
    mpNotPending,
    mpOnBudget,
    mpOnComm,
    mpOnDays,
    mpOnDelay,
    mpPFeeLbl,
    mpPNameLbl,
    mpPctB,
    mpPctD,
    mpPctS,
    mpProvMask,
    mpProviders,
    mpRefB,
    mpRefD,
    mpRefS,
    mpRewardMain,
    mpRewardNote,
    mpSetModeDel,
    mpSetModeMin,
    mpSortFee,
    mpSortName,
    mpSubmit,
    mpSubmitLabel,
    mpSubmitMsg,
    mpSubmitOp,
    mpToggleAccept,
    mpUnitLabel,
    mtCommOff,
    mtCommOp,
    mtCommPe,
    mtDelBg,
    mtDelC,
    mtDelFg,
    mtDelW,
    mtMinBg,
    mtMinC,
    mtMinFg,
    mtMinW,
    mtRadius,
  } = o;
  const dim = { ...LABEL, color: '#ffffff', opacity: 0.7 };
  const modeTab = (onClick, bg, fg, ringWidth, ringColor, name) => (
    <button
      onClick={onClick}
      style={{
        ...BUTTON,
        flex: 1,
        height: 40,
        outline: 'none',
        borderRadius: '14px 14px 0 0',
        background: bg,
        color: fg,
        fontSize: 15,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 8,
      }}
    >
      <span
        style={{
          width: 18,
          height: 18,
          flexShrink: 0,
          borderRadius: 9,
          boxSizing: 'border-box',
          boxShadow: `inset 0 0 0 ${ringWidth}px ${ringColor}`,
          transition: 'box-shadow 160ms',
        }}
      />
      {name}
    </button>
  );
  return (
    <>
      <div style={{ padding: '12px 20px 0', display: 'flex', flexDirection: 'column' }}>
        <Legend title={mpMcTitle} items={mpMintLegend} />
        <div style={{ position: 'relative', width: 350, height: mpMintChartH, marginTop: 26 }}>
          {mpMintChart}
        </div>
        <div style={{ ...CARD, marginTop: 12, padding: '10px 16px 4px', boxShadow: mpBudgetRing }}>
          <div style={SPLIT}>
            <span style={dim}>Budget</span>
            <Amount
              value={mpBudgetVal}
              onChange={mpOnBudget}
              onBlur={mpBlurBudget}
              placeholder="0"
              width={mpBudgetW}
              maxWidth={150}
              unit={mpUnitLabel}
            />
          </div>
          <Slider track={mpRefB} onDown={mpDownB} pct={mpPctB} />
          {mpHasErr ? (
            <span
              style={{
                margin: '-2px 0 4px',
                fontSize: 13,
                lineHeight: '18px',
                fontWeight: 600,
                color: '#ff6b57',
              }}
            >
              {mpErr}
            </span>
          ) : null}
          <div style={{ ...SPLIT, marginTop: 4 }}>
            <span style={dim}>Duration</span>
            <Amount
              value={mpDaysVal}
              onChange={mpOnDays}
              onBlur={mpBlurDays}
              width={mpDaysW}
              maxWidth={120}
              unit={mpDurUnit}
            />
          </div>
          <Slider track={mpRefD} onDown={mpDownD} pct={mpPctD} />
          {mpAdvOpt ? (
            <div style={{ display: 'flex', flexDirection: 'column' }}>
              <div style={{ ...SPLIT, marginTop: 4 }}>
                <span style={dim}>Starts in</span>
                <Amount
                  value={mpDelayVal}
                  onChange={mpOnDelay}
                  onBlur={mpBlurDelay}
                  width={mpDelayW}
                  maxWidth={120}
                  unit={mpDurUnit}
                />
              </div>
              <Slider track={mpRefS} onDown={mpDownS} pct={mpPctS} />
            </div>
          ) : null}
          <div style={{ height: 4 }} />
        </div>
        <div style={{ marginTop: 12, display: 'flex', flexDirection: 'column' }}>
          <div style={{ position: 'relative', display: 'flex', gap: 6 }}>
            {modeTab(mpSetModeMin, mtMinBg, mtMinFg, mtMinW, mtMinC, 'Independent')}
            {modeTab(mpSetModeDel, mtDelBg, mtDelFg, mtDelW, mtDelC, 'Delegation')}
          </div>
          <div style={{ ...CARD, borderRadius: mtRadius, padding: '6px 16px 0' }}>
            <div style={{ height: 142, display: 'flex', flexDirection: 'column' }}>
              {mpIsMinter ? (
                <div style={{ display: 'flex', flexDirection: 'column' }}>
                  <div onClick={mpToggleAccept} style={{ ...SPLIT, height: 48, cursor: 'pointer' }}>
                    <span style={{ fontSize: 15, lineHeight: '20px', fontWeight: 600 }}>
                      Accept delegations
                    </span>
                    <Switch on={mpAccBg} left={mpAccX2} />
                  </div>
                  <div
                    style={{
                      borderTop: HAIRLINE,
                      padding: '8px 0 6px',
                      display: 'flex',
                      flexDirection: 'column',
                      gap: 8,
                      opacity: mtCommOp,
                      pointerEvents: mtCommPe,
                      transition: 'opacity 160ms',
                    }}
                  >
                    <div data-name-field="" style={SPLIT}>
                      <span style={{ ...dim, flexShrink: 0 }}>Display name</span>
                      <NameField o={o} disabled={mtCommOff} pointerEvents={mtCommPe} width={150} />
                    </div>
                    <div style={SPLIT}>
                      <span style={dim}>Your commission</span>
                      <div style={{ display: 'flex', alignItems: 'baseline', gap: 8 }}>
                        <label style={FIELD}>
                          <input
                            value={mpCommVal}
                            onChange={mpOnComm}
                            disabled={mtCommOff}
                            onFocus={mpFocusEnd}
                            onBlur={mpBlurComm}
                            inputMode="decimal"
                            style={{
                              ...INPUT,
                              pointerEvents: mtCommPe,
                              opacity: 1,
                              WebkitTextFillColor: '#ffffff',
                              width: `${mpCommW}ch`,
                              fontVariantNumeric: 'tabular-nums',
                            }}
                          />
                        </label>
                        <span style={dim}>%</span>
                      </div>
                    </div>
                  </div>
                </div>
              ) : null}
              {mpIsDel ? (
                <div
                  style={{
                    height: '100%',
                    display: 'flex',
                    flexDirection: 'column',
                    padding: '6px 0 4px',
                    boxSizing: 'border-box',
                  }}
                >
                  {mpHasProviders ? (
                    <div
                      style={{
                        display: 'flex',
                        justifyContent: 'space-between',
                        gap: 12,
                        marginBottom: 2,
                        fontSize: 13,
                        lineHeight: '16px',
                        color: '#ffffff',
                        opacity: 0.64,
                      }}
                    >
                      <span onClick={mpSortName} style={{ cursor: 'pointer' }}>
                        {mpPNameLbl}
                      </span>
                      <span onClick={mpSortFee} style={{ cursor: 'pointer' }}>
                        {mpPFeeLbl}
                      </span>
                    </div>
                  ) : null}
                  <div
                    style={{
                      maxHeight: 114,
                      overflowY: 'auto',
                      scrollbarWidth: 'none',
                      WebkitMaskImage: mpProvMask,
                      maskImage: mpProvMask,
                    }}
                  >
                    {mpProviders.map((p, i) => (
                      <div
                        key={i}
                        onClick={p.pick}
                        style={{
                          opacity: p.op,
                          display: 'flex',
                          alignItems: 'center',
                          gap: 10,
                          minHeight: 44,
                          cursor: 'pointer',
                        }}
                      >
                        <span
                          style={{
                            width: 20,
                            height: 20,
                            flexShrink: 0,
                            borderRadius: 10,
                            boxSizing: 'border-box',
                            boxShadow: p.ring,
                            background: p.dot,
                            display: 'flex',
                            alignItems: 'center',
                            justifyContent: 'center',
                          }}
                        >
                          <svg
                            width="11"
                            height="9"
                            viewBox="0 0 11 9"
                            fill="none"
                            stroke="#ffffff"
                            strokeWidth="2"
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            style={{ opacity: p.check }}
                          >
                            <path d="M1 4.5 4 7.5 10 1.5" />
                          </svg>
                        </span>
                        <span
                          style={{
                            flex: 1,
                            minWidth: 0,
                            fontSize: 15,
                            lineHeight: '20px',
                            fontWeight: 600,
                            ...ELLIPSIS,
                          }}
                        >
                          {p.name}
                        </span>
                        <span style={{ ...LABEL, fontVariantNumeric: 'tabular-nums' }}>
                          {p.fee}%
                        </span>
                      </div>
                    ))}
                  </div>
                  {mpNoProviders ? (
                    <div
                      style={{
                        flex: 1,
                        display: 'flex',
                        alignItems: 'center',
                        justifyContent: 'center',
                        padding: '0 8px',
                        ...dim,
                        textAlign: 'center',
                        textWrap: 'balance',
                      }}
                    >
                      {mpNoProvText}
                    </div>
                  ) : null}
                </div>
              ) : null}
            </div>
            <div style={{ ...SPLIT, borderTop: HAIRLINE, padding: '8px 0 10px' }}>
              <span style={dim}>Estimated reward</span>
              <span
                style={{
                  display: 'flex',
                  flexDirection: 'column',
                  alignItems: 'flex-end',
                  justifyContent: 'center',
                  gap: 2,
                  minHeight: 40,
                }}
              >
                <span style={{ ...VALUE, color: '#ff3b1f' }}>{mpRewardMain}</span>
                <span style={{ fontSize: 12, lineHeight: '16px', color: '#ffffff', opacity: 0.6 }}>
                  {mpRewardNote}
                </span>
              </span>
            </div>
          </div>
        </div>
        <div style={{ height: 12 }} />
      </div>
      <div
        style={{
          position: 'relative',
          padding: '0 20px 12px',
          display: 'flex',
          flexDirection: 'column',
          gap: 10,
        }}
      >
        <button
          onClick={mpSubmit}
          className="press"
          style={{
            ...BUTTON,
            height: 48,
            borderRadius: 24,
            background: '#0a84ff',
            color: '#ffffff',
            opacity: mpSubmitOp,
            letterSpacing: '-0.41px',
          }}
        >
          {mpNotPending ? mpSubmitLabel : null}
          {mpIsPending ? (
            <span
              style={{
                display: 'inline-block',
                width: 20,
                height: 20,
                boxSizing: 'border-box',
                borderRadius: '50%',
                border: '2.5px solid rgba(255,255,255,0.35)',
                borderTopColor: '#ffffff',
                verticalAlign: 'middle',
                animation: 'mpSpin 0.8s linear infinite',
              }}
            />
          ) : null}
        </button>
        {mpHasSubmitMsg ? (
          <span style={{ fontSize: 13, lineHeight: '18px', color: '#ffcc00', textAlign: 'center' }}>
            {mpSubmitMsg}
          </span>
        ) : null}
        {mpHasPendingReq ? (
          <button
            onClick={mpDiscard}
            style={{
              ...BUTTON,
              alignSelf: 'center',
              height: 32,
              padding: '0 12px',
              borderRadius: 16,
              background: 'transparent',
              color: DIM,
              fontSize: 15,
              fontWeight: 400,
            }}
          >
            Discard pending request
          </button>
        ) : null}
      </div>
    </>
  );
}

/** A burn of the person's own: its number, budget, kind and result at today's mint rate. */
function PortfolioRow({ h }) {
  return (
    <div onClick={h.open} style={{ ...ROW, cursor: 'pointer' }}>
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: 'minmax(0,1fr) auto auto',
          gap: 12,
          alignItems: 'center',
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0 }}>
          <span
            style={{
              flexShrink: 0,
              minWidth: 24,
              fontSize: 15,
              lineHeight: '20px',
              fontWeight: 600,
              color: 'rgba(255,255,255,0.5)',
              fontVariantNumeric: 'tabular-nums',
            }}
          >
            #{h.num}
          </span>
          <div style={COLUMN}>
            <span
              style={{ fontSize: 17, lineHeight: '21px', fontWeight: 600, whiteSpace: 'nowrap' }}
            >
              {h.amt}
              <span style={UNIT}>{h.unit}</span>
            </span>
            <span style={{ fontSize: 13, lineHeight: '16px', color: DIM, ...ELLIPSIS }}>
              {h.kind}
            </span>
          </div>
        </div>
        <span style={{ ...VALUE, color: h.resFg }}>
          {h.resNum}
          <span style={UNIT}>{h.resUnit}</span>
        </span>
        <Chevron color="rgba(255,255,255,0.35)" />
      </div>
    </div>
  );
}

/** Someone's burn in a list: who, how much, its status and progress. With `roles`, the badges too. */
function BurnRow({ k, roles, dimmed }) {
  return (
    <div style={{ ...ROW, background: k.bg, opacity: dimmed ? 0.6 : 1 }}>
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: 'minmax(0,1fr) auto',
          gap: 12,
          alignItems: 'center',
        }}
      >
        <div style={COLUMN}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0 }}>
            <span style={TITLE}>{k.title}</span>
            {roles && k.you ? <span style={YOU}>You</span> : null}
            {roles ? <span style={{ ...ROLE, flexShrink: 0 }}>{k.role}</span> : null}
          </div>
          <span style={{ fontSize: 12, lineHeight: '16px', opacity: 0.7, ...ELLIPSIS }}>
            {k.sub}
          </span>
        </div>
        <span
          style={{
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'flex-end',
            gap: 2,
            whiteSpace: 'nowrap',
          }}
        >
          <span style={{ fontSize: 15, lineHeight: '18px', fontWeight: 600, color: k.sFg }}>
            {k.status}
          </span>
          <span style={{ fontSize: 12, lineHeight: '16px', opacity: 0.7 }}>{k.when}</span>
        </span>
      </div>
      {k.active ? (
        <div style={{ height: 3, borderRadius: 2, background: 'rgba(255,255,255,0.1)' }}>
          <div style={{ height: 3, borderRadius: 2, background: '#30d158', width: `${k.pct}%` }} />
        </div>
      ) : null}
    </div>
  );
}

function PortfolioTab({ o }) {
  const {
    mpAccFlm,
    mpAccRes,
    mpAccResFg,
    mpAccUnit,
    mpBurnsIn,
    mpBurnsOut,
    mpConnectSafely,
    mpDlgIn,
    mpDlgOut,
    mpGoMint,
    mpHasBurns,
    mpHasDlg,
    mpNoBurns,
    mpSafelyLabel,
    mpSafelyOff,
    mpSafelyOp,
    mpSumRes,
    mpSummary,
  } = o;
  return (
    <div
      style={{
        padding: '16px 20px 24px',
        display: 'flex',
        flexDirection: 'column',
        minHeight: '100%',
        boxSizing: 'border-box',
      }}
    >
      <div style={{ ...COLUMN, padding: '0 4px' }}>
        <span style={{ fontSize: 13, lineHeight: '16px', color: DIM }}>Accumulated</span>
        <span
          style={{
            fontSize: 34,
            lineHeight: '41px',
            fontWeight: 700,
            letterSpacing: '-0.02em',
            fontVariantNumeric: 'tabular-nums',
            whiteSpace: 'nowrap',
          }}
        >
          {mpAccFlm}
          <span
            style={{ marginLeft: 6, fontSize: 20, fontWeight: 500, letterSpacing: 0, color: DIM }}
          >
            FLM
          </span>
        </span>
        <span
          style={{
            marginTop: 2,
            display: 'flex',
            alignItems: 'baseline',
            gap: 6,
            fontVariantNumeric: 'tabular-nums',
            whiteSpace: 'nowrap',
          }}
        >
          <span style={LABEL}>Result</span>
          <span style={{ fontSize: 17, lineHeight: '22px', fontWeight: 600, color: mpAccResFg }}>
            {mpAccRes}
            <span style={UNIT}>{mpAccUnit}</span>
          </span>
        </span>
      </div>
      {mpSafelyOff ? (
        <button
          onClick={mpConnectSafely}
          className="press"
          style={{
            ...PRIMARY,
            position: 'relative',
            marginTop: 24,
            padding: '0 52px',
            background: 'linear-gradient(135deg,#0178ff 0%,#0048d2 100%)',
            boxShadow: 'inset 0 0 0 0.5px rgba(255,255,255,0.18),0 6px 20px rgba(1,120,255,0.28)',
            letterSpacing: '-0.41px',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            opacity: mpSafelyOp,
          }}
        >
          <span
            style={{
              position: 'absolute',
              left: 0,
              top: 0,
              width: 52,
              height: 52,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
            }}
          >
            <svg width="30" height="30" viewBox="0 0 96 96" fill="none">
              <circle cx="75.9968" cy="69.0464" r="3.5" fill="#ffffff" />
              <path
                fill="#ffffff"
                d="M35.8443 33.3131C34.061 44 76.4197 36.0681 72.5952 58.3131C70.5 70.5 59.0001 74 46.5 74C34.4763 74 17.8263 69.3738 19.6899 54.3364C19.7939 53.4976 19.8459 53.0782 20.1765 52.8233C20.5072 52.5685 20.9512 52.6285 21.8392 52.7485L36.456 54.7238C37.3957 54.8508 37.8656 54.9142 38.1159 55.2296C38.3662 55.5449 38.3188 56.0447 38.224 57.0441C37.8237 61.2658 39.0734 62.1682 42.095 62.813C50 64.5 59.095 63.0631 60.3433 56.813C62.84 44.313 26.0001 52.5 26.595 34.3131C27.246 14.4099 70.8593 14.0068 67.4346 33.2272C67.2922 34.0268 67.2209 34.4265 66.8707 34.6633C66.5205 34.9001 66.0868 34.8098 65.2194 34.6291L56.6751 32.849C55.9477 32.6974 55.584 32.6217 55.4136 32.3806C55.2431 32.1395 55.3017 31.7074 55.4188 30.8432C55.8877 27.3839 52.848 27 50.0001 27C45.0001 27 36.6901 28.2445 35.8443 33.3131Z"
              />
            </svg>
          </span>
          <span
            style={{
              display: 'flex',
              flexDirection: 'column',
              alignItems: 'center',
              gap: 1,
              textAlign: 'center',
            }}
          >
            <span>{mpSafelyLabel}</span>
            <span
              style={{
                fontSize: 13,
                lineHeight: '16px',
                fontWeight: 500,
                letterSpacing: 0,
                opacity: 0.85,
              }}
            >
              to use your flames in a wallet
            </span>
          </span>
        </button>
      ) : null}
      <div style={{ ...CARD, marginTop: 24, padding: '0 16px' }}>
        {mpSummary.map((r, i) => (
          <div
            key={i}
            style={{
              display: 'grid',
              gridTemplateColumns: 'minmax(0,1fr) auto',
              gap: 16,
              alignItems: 'baseline',
              padding: r.pad,
              borderTop: r.bt,
            }}
          >
            <span
              style={{
                fontSize: r.fs,
                lineHeight: '20px',
                color: r.lfg,
                paddingLeft: r.ind,
                whiteSpace: 'nowrap',
              }}
            >
              {r.l}
            </span>
            <span
              style={{
                fontSize: r.fs,
                lineHeight: '20px',
                fontWeight: 600,
                fontVariantNumeric: 'tabular-nums',
                whiteSpace: 'nowrap',
                textAlign: 'right',
                color: r.fg,
              }}
            >
              {r.v}
            </span>
          </div>
        ))}
        <Result color={mpAccResFg}>{mpSumRes}</Result>
      </div>
      <div style={{ marginTop: 12, display: 'flex', flexDirection: 'column' }}>
        {mpNoBurns ? (
          <div style={{ ...CARD, padding: '0 16px' }}>
            <span style={{ padding: '15px 0 10px', fontSize: 13, lineHeight: '16px', color: DIM }}>
              Burns
            </span>
            <div style={{ ...SPLIT, margin: '0 -16px', padding: '12px 16px', borderTop: HAIRLINE }}>
              <span style={LABEL}>No burns yet</span>
              <span onClick={mpGoMint} style={{ ...PILL, cursor: 'pointer' }}>
                Mint
              </span>
            </div>
          </div>
        ) : null}
        {mpHasBurns ? (
          <div style={LIST}>
            <div>
              <div style={{ ...STICKY, gridTemplateColumns: 'minmax(0,1fr) auto' }}>
                <span>Burns</span>
                <span style={{ textAlign: 'right', paddingRight: 20 }}>Result</span>
              </div>
              {mpBurnsIn.map((h, i) => (
                <PortfolioRow key={i} h={h} />
              ))}
            </div>
            {mpBurnsOut.map((h, i) => (
              <PortfolioRow key={i} h={h} />
            ))}
          </div>
        ) : null}
        {mpHasDlg ? (
          <div style={{ ...LIST, marginTop: 12 }}>
            <div>
              <div style={{ ...STICKY, gridTemplateColumns: 'minmax(0,1fr) auto' }}>
                <span>Delegated to you</span>
                <span style={{ textAlign: 'right' }}>Status</span>
              </div>
              {mpDlgIn.map((h, i) => (
                <BurnRow key={i} k={h} />
              ))}
            </div>
            {mpDlgOut.map((h, i) => (
              <BurnRow key={i} k={h} />
            ))}
          </div>
        ) : null}
      </div>
    </div>
  );
}

function Result({ color, children }) {
  return (
    <div
      style={{
        ...SPLIT,
        margin: '0 -16px',
        padding: '14px 16px',
        borderTop: '1px solid rgba(255,255,255,0.35)',
      }}
    >
      <span style={{ fontSize: 17, lineHeight: '22px', fontWeight: 700 }}>Result</span>
      <span
        style={{
          fontSize: 26,
          lineHeight: '30px',
          fontWeight: 700,
          letterSpacing: '-0.02em',
          fontVariantNumeric: 'tabular-nums',
          whiteSpace: 'nowrap',
          color,
        }}
      >
        {children}
      </span>
    </div>
  );
}

function SettingsTab({ o }) {
  const {
    mpAdvOptBg,
    mpAdvOptX,
    mpAskReset,
    mpPrevWallets,
    mpRepeatTutorial,
    mpSwBlocks,
    mpSwBlocksBg,
    mpSwBlocksFg,
    mpSwBtc,
    mpSwBtcBg,
    mpSwBtcFg,
    mpSwDays,
    mpSwDaysBg,
    mpSwDaysFg,
    mpSwSat,
    mpSwSatBg,
    mpSwSatFg,
    mpSwUsd,
    mpSwUsdBg,
    mpSwUsdFg,
    mpToggleAdvOpt,
  } = o;
  const setting = { ...SPLIT, minHeight: 52, padding: '0 16px' };
  const name = { fontSize: 15, lineHeight: '20px', fontWeight: 600 };
  const choice = (onClick, bg, fg, text) => (
    <button
      onClick={onClick}
      style={{
        ...BUTTON,
        height: 32,
        padding: '0 12px',
        borderRadius: 7,
        background: bg,
        color: fg,
        fontSize: 13,
      }}
    >
      {text}
    </button>
  );
  const choices = (children) => (
    <div style={{ display: 'flex', gap: 2, padding: 2, borderRadius: 9, background: '#2c2c31' }}>
      {children}
    </div>
  );
  return (
    <div style={{ padding: '16px 20px 24px', display: 'flex', flexDirection: 'column' }}>
      <div style={{ ...CARD, marginBottom: 16, overflow: 'clip' }}>
        <div data-name-field="" style={setting}>
          <span style={{ ...name, flexShrink: 0 }}>Display name</span>
          <NameField o={o} width={170} padding="4px 10px" />
        </div>
      </div>
      <div style={{ ...CARD, marginBottom: 16, overflow: 'clip' }}>
        <div style={setting}>
          <span style={name}>Currency</span>
          {choices(
            <>
              {choice(mpSwSat, mpSwSatBg, mpSwSatFg, 'sat')}
              {choice(mpSwBtc, mpSwBtcBg, mpSwBtcFg, 'BTC')}
              {choice(mpSwUsd, mpSwUsdBg, mpSwUsdFg, 'USD')}
            </>,
          )}
        </div>
        <div style={{ ...setting, borderTop: HAIRLINE }}>
          <span style={name}>Time</span>
          {choices(
            <>
              {choice(mpSwBlocks, mpSwBlocksBg, mpSwBlocksFg, 'blocks')}
              {choice(mpSwDays, mpSwDaysBg, mpSwDaysFg, 'days')}
            </>,
          )}
        </div>
        <div
          onClick={mpToggleAdvOpt}
          style={{
            ...setting,
            padding: '10px 16px',
            boxSizing: 'border-box',
            borderTop: HAIRLINE,
            cursor: 'pointer',
          }}
        >
          <div style={COLUMN}>
            <span style={name}>Advanced options</span>
            <span style={{ fontSize: 13, lineHeight: '16px', color: 'rgba(255,255,255,0.6)' }}>
              Delayed start on the Mint tab
            </span>
          </div>
          <Switch on={mpAdvOptBg} left={mpAdvOptX} />
        </div>
      </div>
      <div data-set-card="" style={{ ...CARD, overflow: 'clip' }}>
        <button
          onClick={mpRepeatTutorial}
          aria-label="Repeat tutorial"
          style={{
            ...BUTTON,
            ...SPLIT,
            width: '100%',
            background: 'transparent',
            minHeight: 48,
            padding: '0 16px',
            color: '#ffffff',
            fontSize: 15,
            borderTop: HAIRLINE,
          }}
        >
          <span>Tutorial</span>
          <span
            style={{
              flexShrink: 0,
              width: 76,
              boxSizing: 'border-box',
              height: 32,
              padding: 0,
              border: 'none',
              borderRadius: 16,
              font: 'inherit',
              fontSize: 15,
              lineHeight: '32px',
              fontWeight: 600,
              textAlign: 'center',
              whiteSpace: 'nowrap',
              cursor: 'pointer',
              background: '#2c2c31',
              color: '#ffffff',
            }}
          >
            Repeat
          </span>
        </button>
      </div>
      <p
        style={{
          margin: '12px 0 0',
          textAlign: 'center',
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
      <div style={{ ...CARD, marginTop: 20, overflow: 'clip' }}>
        {mpPrevWallets.map((w, i) => (
          <div
            key={i}
            onClick={w.pick}
            style={{
              ...SPLIT,
              minHeight: 64,
              padding: '10px 16px',
              boxSizing: 'border-box',
              borderTop: w.bt,
              cursor: w.cur,
            }}
          >
            <div style={COLUMN}>
              <span style={{ fontSize: 17, lineHeight: '22px', fontWeight: 600, ...ELLIPSIS }}>
                {w.name}
              </span>
              <span
                style={{
                  fontSize: 15,
                  lineHeight: '20px',
                  color: 'rgba(255,255,255,0.55)',
                  whiteSpace: 'nowrap',
                }}
              >
                {w.sub}
              </span>
            </div>
            {w.isCur ? <Check color="#0a84ff" width="2.4" /> : null}
          </div>
        ))}
      </div>
      <button
        onClick={mpAskReset}
        style={{
          ...BUTTON,
          alignSelf: 'center',
          marginTop: 16,
          height: 44,
          padding: '0 22px',
          borderRadius: 22,
          background: '#1a1a1d',
          color: '#ffffff',
        }}
      >
        Create new minter
      </button>
    </div>
  );
}

function TabBar({ o }) {
  const {
    mpGoLive,
    mpGoMe,
    mpGoMint,
    mpGoSet,
    mpTabColLive,
    mpTabColMe,
    mpTabColMint,
    mpTabColSet,
    mpTabFillMint,
    mpTabInnerMint,
    mpTabSwLive,
    mpTabSwMe,
    mpTabSwSet,
  } = o;
  const tab = (onClick, color, name, icon) => (
    <button
      onClick={onClick}
      style={{
        ...BUTTON,
        flex: 1,
        background: 'transparent',
        outline: 'none',
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        gap: 3,
        paddingTop: 8,
        color,
        fontSize: 12,
      }}
    >
      {icon}
      <span>{name}</span>
    </button>
  );
  const icon = (width, children, fill = 'none') => (
    <svg
      width="24"
      height="24"
      viewBox="0 0 24 24"
      fill={fill}
      stroke="currentColor"
      strokeWidth={width}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {children}
    </svg>
  );
  return (
    <div
      style={{
        position: 'relative',
        zIndex: 5,
        flexShrink: 0,
        height: 'var(--tb)',
        boxSizing: 'border-box',
        display: 'flex',
        background: '#141416',
        borderTop: '0.5px solid rgba(255,255,255,0.16)',
      }}
    >
      {tab(
        mpGoLive,
        mpTabColLive,
        'Network',
        icon(
          mpTabSwLive,
          <>
            <circle cx="12" cy="12" r="9" />
            <path d="M3 12h18" />
            <path d="M12 3c2.4 2.5 3.6 5.5 3.6 9s-1.2 6.5-3.6 9c-2.4-2.5-3.6-5.5-3.6-9s1.2-6.5 3.6-9z" />
          </>,
        ),
      )}
      {tab(
        mpGoMint,
        mpTabColMint,
        'Mint',
        <svg
          width="24"
          height="24"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth="1.7"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path
            d="M12 2.2c.4 3 2.4 4.8 3.9 6.6 1.4 1.6 2.3 3.3 2.3 5.4a6.2 6.2 0 0 1-12.4 0c0-2.2 1-3.9 2.3-5.3.3 1.3 1 2.2 2 2.6-.3-2.9.4-5.9 1.9-9.3z"
            fill={mpTabFillMint}
          />
          <path
            d="M12 20.4a2.9 2.9 0 0 1-2.9-2.9c0-1.5.9-2.5 1.8-3.5.2.9.6 1.4 1.2 1.7.5-.9.8-1.9.7-3.1 1.3 1.1 2.1 2.7 2.1 4.3a2.9 2.9 0 0 1-2.9 3.5z"
            fill={mpTabInnerMint}
          />
        </svg>,
      )}
      {tab(
        mpGoMe,
        mpTabColMe,
        'Portfolio',
        icon(
          mpTabSwMe,
          <>
            <path d="M3 17.5l5.5-5.5 4 3.5L21 7" />
            <path d="M15.5 7H21v5.5" />
          </>,
        ),
      )}
      {tab(
        mpGoSet,
        mpTabColSet,
        'Settings',
        icon(
          mpTabSwSet,
          <>
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </>,
        ),
      )}
    </div>
  );
}

// --- Sheets and pages -------------------------------------------------------------------------------

/** A sheet rising from the bottom over a dimmed screen; a tap beside it closes it. */
function Sheet({ onClose, padding, children }) {
  return (
    <div
      style={{
        position: 'absolute',
        inset: 0,
        zIndex: 30,
        display: 'flex',
        flexDirection: 'column',
        justifyContent: 'flex-end',
        padding,
      }}
    >
      <div
        onClick={onClose}
        style={{
          position: 'absolute',
          inset: 0,
          background: 'rgba(0,0,0,0.55)',
          animation: 'mpFadeIn 220ms ease both',
        }}
      />
      {children}
    </div>
  );
}

function ConfirmSheet({ o }) {
  const { mpCf, mpConfirmCancel, mpConfirmGo } = o;
  const line = (name, value) => (
    <div style={{ ...SPLIT, minHeight: 44, borderTop: HAIRLINE }}>
      <span style={LABEL}>{name}</span>
      <span style={{ ...VALUE, fontSize: 15, lineHeight: '20px' }}>{value}</span>
    </div>
  );
  return (
    <Sheet onClose={mpConfirmCancel}>
      <div
        style={{
          position: 'relative',
          background: '#1c1c1e',
          borderRadius: '16px 16px 0 0',
          padding: '8px 16px calc(var(--sb) + 4px)',
          display: 'flex',
          flexDirection: 'column',
          animation: 'mpSheetIn 340ms cubic-bezier(0.2,0.9,0.25,1) both',
        }}
      >
        <div
          style={{
            width: 36,
            height: 5,
            borderRadius: 3,
            background: 'rgba(255,255,255,0.3)',
            margin: '0 auto 14px',
          }}
        />
        <span
          style={{ fontSize: 22, lineHeight: '28px', fontWeight: 700, letterSpacing: '-0.01em' }}
        >
          Confirm burn
        </span>
        <span style={{ ...LABEL, margin: '4px 0 12px' }}>Burning bitcoins is not reversible.</span>
        {line('How you mint', mpCf.mode)}
        {line('Burn', mpCf.burn)}
        {line(mpCf.feeLabel, mpCf.fees)}
        {line('Duration', mpCf.dur)}
        {mpCf.later ? line(mpCf.startsLabel, mpCf.starts) : null}
        <button onClick={mpConfirmGo} style={{ ...PRIMARY, marginTop: 16 }}>
          Confirm
        </button>
        <button
          onClick={mpConfirmCancel}
          style={{ ...BUTTON, height: 48, background: 'transparent', color: '#0a84ff' }}
        >
          Cancel
        </button>
      </div>
    </Sheet>
  );
}

function ResetSheet({ o }) {
  const { mpResetCancel, mpResetGo } = o;
  return (
    <Sheet onClose={mpResetCancel} padding="0 8px calc(var(--sb) + 4px)">
      <div
        style={{
          position: 'relative',
          display: 'flex',
          flexDirection: 'column',
          gap: 8,
          animation: 'mpSheetIn 340ms cubic-bezier(0.2,0.9,0.25,1) both',
        }}
      >
        <div
          style={{
            background: '#2c2c2e',
            borderRadius: 13,
            overflow: 'clip',
            display: 'flex',
            flexDirection: 'column',
          }}
        >
          <div
            style={{
              padding: '16px 16px 14px',
              textAlign: 'center',
              display: 'flex',
              flexDirection: 'column',
              gap: 4,
            }}
          >
            <span style={{ fontSize: 13, lineHeight: '18px', fontWeight: 600, color: DIM }}>
              Create new minter?
            </span>
            <span style={{ fontSize: 13, lineHeight: '18px', color: 'rgba(255,255,255,0.6)' }}>
              You’ll start with an empty portfolio. This minter stays in the list.
            </span>
          </div>
          <button
            onClick={mpResetGo}
            style={{
              ...BUTTON,
              height: 56,
              borderTop: HAIRLINE,
              background: 'transparent',
              color: '#ff453a',
              fontWeight: 400,
            }}
          >
            Create minter
          </button>
        </div>
        <button
          onClick={mpResetCancel}
          style={{
            ...BUTTON,
            height: 56,
            borderRadius: 13,
            background: '#2c2c2e',
            color: '#0a84ff',
          }}
        >
          Cancel
        </button>
      </div>
    </Sheet>
  );
}

/** A page pushed in over the tabs, with a way back to where it came from. */
function Page({ back, onBack, title, children }) {
  return (
    <div
      data-noscroll=""
      style={{
        position: 'absolute',
        left: 0,
        right: 0,
        top: 'var(--mst)',
        bottom: 'var(--tb)',
        zIndex: 15,
        boxShadow: '0 calc(-1 * var(--mst)) 0 0 #0c0c0d',
        background: '#0c0c0d',
        overflowY: 'auto',
        overflowX: 'hidden',
        scrollbarWidth: 'none',
        animation: 'mpPushIn 280ms cubic-bezier(0.2,0.9,0.25,1) both',
      }}
    >
      <div style={{ padding: '6px 20px 24px', display: 'flex', flexDirection: 'column', gap: 8 }}>
        <button
          onClick={onBack}
          style={{
            ...BUTTON,
            alignSelf: 'flex-start',
            display: 'flex',
            alignItems: 'center',
            gap: 4,
            height: 44,
            marginLeft: -6,
            padding: '0 6px',
            background: 'transparent',
            color: '#0a84ff',
            fontWeight: 400,
          }}
        >
          <svg
            width="12"
            height="20"
            viewBox="0 0 12 20"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.4"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M10 2 2 10l8 8" />
          </svg>
          {back}
        </button>
        {title}
        {children}
      </div>
    </div>
  );
}

const H1 = {
  margin: '0 0 8px',
  fontSize: 28,
  lineHeight: '34px',
  fontWeight: 700,
  letterSpacing: '-0.02em',
};

function BurnDetail({ o }) {
  const { mpBD, mpCloseSub } = o;
  const lines = (rows) => (
    <div style={{ display: 'flex', flexDirection: 'column', marginTop: -0.5 }}>
      {rows.map((r, i) => (
        <div
          key={i}
          style={{
            ...SPLIT,
            minHeight: 44,
            padding: '6px 0',
            boxSizing: 'border-box',
            borderTop: '0.5px solid rgba(255,255,255,0.12)',
          }}
        >
          <span style={LABEL}>{r.l}</span>
          <span style={{ ...VALUE, fontSize: 15, lineHeight: '20px' }}>{r.v}</span>
        </div>
      ))}
    </div>
  );
  return (
    <Page
      back="Portfolio"
      onBack={mpCloseSub}
      title={<h1 style={{ ...H1, margin: 0 }}>{mpBD.title}</h1>}
    >
      <span style={{ ...LABEL, margin: '0 0 8px' }}>{mpBD.sub}</span>
      <div style={{ ...CARD, padding: '0 16px', overflow: 'clip' }}>
        {lines(mpBD.g1)}
        <div
          style={{
            margin: '0 -16px',
            padding: '0 16px',
            borderTop: '1px solid rgba(255,255,255,0.35)',
            overflow: 'hidden',
          }}
        >
          {lines(mpBD.g2)}
        </div>
        <Result color={mpBD.resFg}>{mpBD.result}</Result>
      </div>
    </Page>
  );
}

const MINTER_COLUMNS = 'minmax(0,1fr) 56px 52px 52px';
const CELL = { textAlign: 'right', fontSize: 15, lineHeight: '18px', whiteSpace: 'nowrap' };

function MinterRow({ m }) {
  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: MINTER_COLUMNS,
        gap: 8,
        alignItems: 'center',
        minHeight: 60,
        padding: '8px 16px',
        boxSizing: 'border-box',
        borderTop: HAIRLINE,
        margin: '0 -16px',
        background: m.bg,
        fontVariantNumeric: 'tabular-nums',
      }}
    >
      <div style={COLUMN}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0 }}>
          <span style={TITLE}>{m.name}</span>
          {m.you ? <span style={YOU}>You</span> : null}
        </div>
        <span
          style={{
            ...ROLE,
            alignSelf: 'flex-start',
            maxWidth: '100%',
            boxSizing: 'border-box',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
          }}
        >
          {m.badge}
        </span>
      </div>
      <span style={CELL}>{m.burnedC}</span>
      <span style={CELL}>{m.earnedN}</span>
      <span style={CELL}>{m.rateN}</span>
    </div>
  );
}

function MintersPage({ o }) {
  const { mpCloseSub, mpMinterCardsHead, mpMinterCardsTail, mpRateHdr } = o;
  const header = (name, by, right) => (
    <span
      onClick={o['mpSortBy' + by]}
      style={{
        textAlign: right ? 'right' : undefined,
        whiteSpace: 'nowrap',
        cursor: 'pointer',
        color: o['mpSort' + by + 'Fg'],
        fontWeight: o['mpSort' + by + 'W'],
      }}
    >
      {name}
    </span>
  );
  return (
    <Page back="Network" onBack={mpCloseSub} title={<h1 style={H1}>Minters</h1>}>
      <div style={LIST}>
        <div>
          <div
            style={{ ...STICKY, gridTemplateColumns: MINTER_COLUMNS, gap: 8, alignItems: 'center' }}
          >
            {header('Minter', 'Name')}
            {header('Burned', 'Burned', true)}
            {header('FLM', 'Earned', true)}
            {header(mpRateHdr, 'Rate', true)}
          </div>
          {mpMinterCardsHead.map((m, i) => (
            <MinterRow key={i} m={m} />
          ))}
        </div>
        {mpMinterCardsTail.map((m, i) => (
          <MinterRow key={i} m={m} />
        ))}
      </div>
    </Page>
  );
}

function BurnsPage({ o }) {
  const { mpCMainIn, mpCMainOut, mpCloseSub, mpHasContribs } = o;
  return (
    <Page back="Network" onBack={mpCloseSub} title={<h1 style={H1}>Burns</h1>}>
      {mpHasContribs ? (
        <div style={LIST}>
          <div>
            <div style={{ ...STICKY, gridTemplateColumns: 'minmax(0,1fr) auto' }}>
              <span>Burn</span>
              <span style={{ textAlign: 'right' }}>Status</span>
            </div>
            {mpCMainIn.map((k, i) => (
              <BurnRow key={i} k={k} roles />
            ))}
          </div>
          {mpCMainOut.map((k, i) => (
            <BurnRow key={i} k={k} roles />
          ))}
        </div>
      ) : null}
    </Page>
  );
}

function Toast({ o }) {
  const { mpHideToast, mpToastAnim, mpToastT1 } = o;
  return (
    <div style={{ ...TOP, zIndex: 16 }}>
      <div
        onClick={mpHideToast}
        style={{ ...TOAST, pointerEvents: 'auto', cursor: 'pointer', animation: mpToastAnim }}
      >
        <span>{mpToastT1}</span>
      </div>
    </div>
  );
}

function OfflineBanner() {
  return (
    <>
      <div
        style={{
          position: 'absolute',
          inset: 0,
          zIndex: 40,
          background: 'rgba(0,0,0,0.55)',
          animation: 'mpFadeIn 200ms ease both',
        }}
      />
      <div style={{ ...TOP, zIndex: 41 }}>
        <div style={{ ...TOAST, animation: 'mpToastIn 380ms cubic-bezier(0.2,0.9,0.3,1.2) both' }}>
          <span>Reconnecting</span>
          <span style={{ display: 'inline-block', width: '1em', textAlign: 'left' }}>
            <span>.</span>
            <span style={{ animation: 'mpDot2 1.2s steps(1) infinite' }}>.</span>
            <span style={{ animation: 'mpDot3 1.2s steps(1) infinite' }}>.</span>
          </span>
        </div>
      </div>
    </>
  );
}

// --- The screen ---------------------------------------------------------------------------------------

export function PlaygroundScreen({ o }) {
  const {
    mpConfirmOn,
    mpIsLiveB,
    mpIsMeB,
    mpIsMintB,
    mpIsSet,
    mpMore,
    mpOfflineBanner,
    mpOnScroll,
    mpResetAskOn,
    mpScroller,
    mpSubBurn,
    mpSubContribs,
    mpSubMinters,
    mpToastOn,
  } = o;
  return (
    <div
      data-screen-label="M Mint Playground"
      style={{
        position: 'absolute',
        left: '50%',
        top: 0,
        width: 390,
        height: 'var(--lh)',
        marginLeft: -195,
        transform: 'scale(var(--s))',
        transformOrigin: '50% 0',
        overflow: 'hidden',
        background: 'transparent',
        display: 'flex',
        flexDirection: 'column',
        color: '#ffffff',
      }}
    >
      <div style={{ position: 'absolute', inset: 0, background: '#0c0c0d' }} />
      <div style={{ height: 'var(--mst)', flexShrink: 0 }} />
      <div
        ref={mpScroller}
        onScroll={mpOnScroll}
        data-noscroll=""
        style={{
          position: 'relative',
          flex: 1,
          minHeight: 0,
          overflowY: 'auto',
          overflowX: 'hidden',
          scrollbarWidth: 'none',
        }}
      >
        {mpIsLiveB ? <NetworkTab o={o} /> : null}
        {mpIsMintB ? <MintTab o={o} /> : null}
        {mpIsMeB ? <PortfolioTab o={o} /> : null}
        {mpIsSet ? <SettingsTab o={o} /> : null}
      </div>
      {/* A soft fade above the tab bar while there is more below. */}
      <div
        style={{
          position: 'absolute',
          left: 0,
          right: 0,
          bottom: 'var(--tb)',
          height: 40,
          zIndex: 4,
          pointerEvents: 'none',
          background: 'linear-gradient(180deg,rgba(12,12,13,0),#0c0c0d)',
          opacity: mpMore ? 1 : 0,
          transition: mpMore ? 'none' : 'opacity 200ms ease',
        }}
      />
      <TabBar o={o} />
      {mpConfirmOn ? <ConfirmSheet o={o} /> : null}
      {mpResetAskOn ? <ResetSheet o={o} /> : null}
      {mpSubBurn ? <BurnDetail o={o} /> : null}
      {mpSubMinters ? <MintersPage o={o} /> : null}
      {mpSubContribs ? <BurnsPage o={o} /> : null}
      {mpToastOn ? <Toast o={o} /> : null}
      {mpOfflineBanner ? <OfflineBanner /> : null}
    </div>
  );
}
