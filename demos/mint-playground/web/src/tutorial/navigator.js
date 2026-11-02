// Tutorial navigation: which screen is shown, the dissolve between screens, swipe and arrow keys,
// the URL hash, and the phone frame on a desktop. The screens themselves are React; this drives
// the few things that are not (CSS variables on <html>, the stage's opacity, history).
const DESIGN_W = 390;
const MIN_H = 782;
const FADE_OUT = 120;
const FADE_IN = 200;

/**
 * `count` screens, the playground being the one at `demoIndex`; `stage()` is the element the
 * screens dissolve in; `onShow(index)` is called whenever another screen is displayed.
 */
export function createNavigator({ count, demoIndex, stage, onShow }) {
  const root = document.documentElement;
  // The Mint Playground is not part of the tutorial: it is opened by "Start Demo" only, has its
  // own scrolling and inputs, and is left with the browser's Back.
  const tutLast = count - 2;
  let idx = 0;
  let target = null;
  let W = innerWidth;
  let demoPushed = false;
  let restartAfterBack = false;
  const inDemo = () => idx === demoIndex;

  // On a desktop the page is shown as a phone: body becomes a 390x844 frame scaled to fit.
  const DESKTOP = matchMedia('(hover:hover) and (pointer:fine) and (min-width:700px)');
  function fit() {
    let H;
    if (DESKTOP.matches) {
      W = DESIGN_W;
      H = 844;
      root.style.setProperty('--k', Math.min(1, (innerHeight - 64) / H, (innerWidth - 64) / W));
    } else {
      W = innerWidth;
      H = innerHeight;
    }
    // The playground scrolls, so it always fills the width; tutorial screens must fit the height.
    const s = inDemo() && !DESKTOP.matches ? W / DESIGN_W : Math.min(W / DESIGN_W, H / MIN_H);
    root.style.setProperty('--s', s);
    root.style.setProperty('--lh', H / s + 'px');
  }

  // Transition: only the content dissolves into the background (out, swap, in). The bottom bars
  // sit beneath it in their own layer and never blink.
  let fadeAnim = null;
  let swapTimer = null;
  const fadeLevel = () => {
    const c = stage();
    return c ? 1 - +getComputedStyle(c).opacity : 0;
  };
  function fadeTo(o, ms, easing) {
    const fade = stage();
    if (!fade) return;
    const from = 1 - fadeLevel();
    if (fadeAnim) fadeAnim.cancel();
    fade.style.opacity = 1 - o;
    fadeAnim =
      ms > 0
        ? fade.animate([{ opacity: from }, { opacity: 1 - o }], { duration: ms, easing })
        : null;
  }

  const pos = () => (target === null ? idx : target);

  function go(i) {
    if (i !== demoIndex) i = Math.max(0, Math.min(tutLast, i));
    if (i === pos()) {
      if (target === null) fadeTo(0, FADE_IN, 'ease-out');
      return;
    }
    target = i;
    const ms = Math.round(FADE_OUT * (1 - fadeLevel()));
    fadeTo(1, ms, 'ease-in');
    clearTimeout(swapTimer);
    swapTimer = setTimeout(swap, ms);
  }

  function show() {
    root.toggleAttribute('data-demo', inDemo());
    fit();
    onShow(idx);
  }

  function swap() {
    idx = target;
    target = null;
    show();
    if (inDemo()) {
      if (location.hash !== '#demo') {
        history.pushState(null, '', '#demo');
        demoPushed = true;
      }
      // Whoever has opened the playground once skips the tutorial on later visits.
      try {
        localStorage.setItem('flame.seenDemo', '1');
      } catch {
        /* remembered only when storage allows */
      }
    } else {
      demoPushed = false;
      history.replaceState(null, '', idx ? '#' + idx : location.pathname + location.search);
      scrollTo(0, 0);
    }
    fadeTo(0, FADE_IN, 'ease-out');
  }

  // A swipe must not also count as a tap on whatever ends up under the finger.
  let suppressUntil = 0;
  const onClick = (e) => {
    if (performance.now() < suppressUntil) {
      e.preventDefault();
      e.stopPropagation();
    }
  };
  const onKey = (e) => {
    if (inDemo()) return;
    if (e.key === 'ArrowRight') go(pos() + 1);
    else if (e.key === 'ArrowLeft') go(pos() - 1);
  };

  // Swipe: nothing moves; the content fades with the finger, release switches.
  let p = null;
  const onDown = (e) => {
    if (
      inDemo() ||
      !e.isPrimary ||
      target !== null ||
      (e.target.closest && e.target.closest('[data-noswipe]'))
    )
      return;
    p = { id: e.pointerId, x: e.clientX, y: e.clientY, t: performance.now(), axis: null, dx: 0 };
  };
  const onMove = (e) => {
    if (!p || e.pointerId !== p.id) return;
    const dx = e.clientX - p.x;
    const dy = e.clientY - p.y;
    if (!p.axis) {
      if (Math.hypot(dx, dy) < 8) return;
      p.axis = Math.abs(dx) > Math.abs(dy) ? 'x' : 'y';
    }
    if (p.axis !== 'x') return;
    const edge = (idx === 0 && dx > 0) || (idx === tutLast && dx < 0);
    p.dx = edge ? dx / 3 : dx;
    fadeTo(Math.min(0.6, (Math.abs(p.dx) / W) * 0.9), 0);
  };
  const onUp = (e) => {
    if (!p || e.pointerId !== p.id) return;
    const q = p;
    p = null;
    if (q.axis !== 'x') return;
    if (Math.abs(q.dx) > 8) suppressUntil = performance.now() + 350;
    const v = q.dx / Math.max(1, performance.now() - q.t);
    if (q.dx < -W * 0.18 || v < -0.45) go(idx + 1);
    else if (q.dx > W * 0.18 || v > 0.45) go(idx - 1);
    else fadeTo(0, 180, 'ease-out');
  };

  // Nothing in the tutorial may scroll (focus/scrollIntoView would shift the screens).
  // The playground scrolls inside itself, and iOS may shift the page for its keyboard.
  const onScroll = (e) => {
    if (inDemo()) return;
    const t = e.target === document ? document.scrollingElement : e.target;
    if (t && (t.scrollLeft || t.scrollTop)) {
      t.scrollLeft = 0;
      t.scrollTop = 0;
    }
  };

  const fromHash = () =>
    location.hash === '#demo' ? demoIndex : parseInt(location.hash.slice(1), 10) || 0;
  const onHash = () => {
    go(restartAfterBack ? 0 : fromHash());
    restartAfterBack = false;
  };
  const onOrientation = () => setTimeout(fit, 250);

  document.addEventListener('click', onClick, true);
  addEventListener('keydown', onKey);
  addEventListener('pointerdown', onDown);
  addEventListener('pointermove', onMove);
  addEventListener('pointerup', onUp);
  addEventListener('pointercancel', onUp);
  addEventListener('scroll', onScroll, true);
  addEventListener('hashchange', onHash);
  addEventListener('resize', fit);
  addEventListener('orientationchange', onOrientation);

  idx = fromHash();
  let seen = false;
  try {
    seen = localStorage.getItem('flame.seenDemo') === '1';
  } catch {
    /* then the tutorial comes first */
  }
  if (!location.hash && seen) {
    idx = demoIndex;
    history.replaceState(null, '', '#demo');
  }
  if (idx !== demoIndex) idx = Math.max(0, Math.min(tutLast, idx));
  show();

  return {
    go,
    pos,
    inDemo,
    back: () => go(pos() - 1),
    next: () => go(pos() + 1),
    start: () => go(1),
    startDemo: () => go(demoIndex),
    // From the playground: leave the demo (dropping its history entry) and start over.
    repeatTutorial: () => {
      if (inDemo() && demoPushed) {
        restartAfterBack = true;
        history.back();
      } else go(0);
    },
    dispose: () => {
      clearTimeout(swapTimer);
      if (fadeAnim) fadeAnim.cancel();
      document.removeEventListener('click', onClick, true);
      removeEventListener('keydown', onKey);
      removeEventListener('pointerdown', onDown);
      removeEventListener('pointermove', onMove);
      removeEventListener('pointerup', onUp);
      removeEventListener('pointercancel', onUp);
      removeEventListener('scroll', onScroll, true);
      removeEventListener('hashchange', onHash);
      removeEventListener('resize', fit);
      removeEventListener('orientationchange', onOrientation);
    },
  };
}
