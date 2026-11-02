// A phone-sized introduction to Flame followed by the Mint Playground. Exactly one screen is
// displayed at a time; the navigator dissolves between them and keeps the URL hash.
import { useEffect, useRef, useState } from 'react';
import { createNavigator } from './tutorial/navigator.js';
import {
  BurnAmount,
  BurnDuration,
  FinishRow,
  FlameVM,
  Intro,
  IntroRow,
  Privacy,
  ProofOfBurn,
  Speed,
  StepRow,
  TryIt,
  TwoWays,
  WhyFlame,
  useTutorialSliders,
} from './tutorial/slides.jsx';
import Playground from './playground/Playground.jsx';

const CELL = {
  position: 'relative',
  flex: '0 0 100%',
  width: '100%',
  height: '100%',
  overflow: 'hidden',
};

export default function App() {
  const [shown, setShown] = useState(-1);
  const stage = useRef(null);
  const navigator = useRef(null);
  const sliders = useTutorialSliders();

  // The handlers below run after mount, when the navigator exists.
  const nav = () => navigator.current;
  const screens = [
    <Intro onStart={() => nav().start()} />,
    <WhyFlame />,
    <ProofOfBurn />,
    <Privacy />,
    <Speed />,
    <FlameVM />,
    <BurnAmount burn={sliders.burn} setBurn={sliders.setBurn} />,
    <BurnDuration burn={sliders.burn} days={sliders.days} setDays={sliders.setDays} />,
    <TwoWays
      budget={sliders.budget}
      setBudget={sliders.setBudget}
      days={sliders.days9}
      setDays={sliders.setDays9}
    />,
    <TryIt onBack={() => nav().back()} onStart={() => nav().startDemo()} />,
    <Playground onRepeatTutorial={() => nav().repeatTutorial()} />,
  ];
  const demoIndex = screens.length - 1;
  const lastStep = demoIndex - 1;

  useEffect(() => {
    const created = createNavigator({
      count: screens.length,
      demoIndex,
      stage: () => stage.current,
      onShow: setShown,
    });
    navigator.current = created;
    return () => {
      created.dispose();
      navigator.current = null;
    };
    // The screen count never changes.
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // The bottom row of the shown tutorial screen, in its own layer beneath the content.
  const row = shown === demoIndex ? -1 : shown === 0 ? 0 : shown === lastStep ? 2 : 1;
  const on = (n) => (row === n ? 'row on' : 'row');

  return (
    <>
      <div id="bg">
        <div className="sheet" />
      </div>
      <div id="edge-top" />
      <div id="chrome">
        <div className="frame">
          <IntroRow className={on(0)} onStart={() => nav().start()} />
          <StepRow
            className={on(1)}
            active={shown - 1}
            onBack={() => nav().back()}
            onNext={() => nav().next()}
          />
          <FinishRow
            className={on(2)}
            onBack={() => nav().back()}
            onStart={() => nav().startDemo()}
          />
        </div>
      </div>
      <div id="stage" ref={stage}>
        <section style={{ height: '100%' }}>
          <div style={{ display: 'flex', flexWrap: 'nowrap', height: '100%' }}>
            {screens.map((screen, i) => (
              <div
                key={i}
                data-cell=""
                style={{ ...CELL, display: i === shown ? 'block' : 'none' }}
              >
                {screen}
              </div>
            ))}
          </div>
        </section>
      </div>
    </>
  );
}
