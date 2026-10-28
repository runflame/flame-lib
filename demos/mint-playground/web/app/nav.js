(() => {
  const DESIGN_W = 390, MIN_H = 782, FLAME_URL = 'https://runflame.org';
  const FADE_OUT = 120, FADE_IN = 200;
  const root = document.documentElement;
  let slides = [], idx = 0, target = null, W = innerWidth;
  // The Mint Playground ("M …" screen) is not part of the tutorial: it is opened by "Start Demo"
  // only, has its own scrolling and inputs, and is left with the browser's Back.
  let tutLast = 0, demoIdx = -1, demoPushed = false;
  const inDemo = () => idx === demoIdx;
  let rows = [], closeBtn = null, dots = [];

  // On a desktop the page is shown as a phone: body becomes a 390x844 frame scaled to fit.
  const DESKTOP = matchMedia('(hover:hover) and (pointer:fine) and (min-width:700px)');
  function fit() {
    let H;
    if (DESKTOP.matches) {
      W = DESIGN_W; H = 844;
      root.style.setProperty('--k', Math.min(1, (innerHeight - 64) / H, (innerWidth - 64) / W));
    } else { W = innerWidth; H = innerHeight; }
    // The playground scrolls, so it always fills the width; tutorial screens must fit the height.
    const s = inDemo() && !DESKTOP.matches ? W / DESIGN_W : Math.min(W / DESIGN_W, H / MIN_H);
    root.style.setProperty('--s', s);
    root.style.setProperty('--lh', (H / s) + 'px');
  }

  // The runtime may re-create the DOM, so visibility lives in a stylesheet keyed by position
  // rather than on the nodes themselves. Exactly one screen is displayed at a time.
  const visSheet = document.createElement('style');
  document.head.appendChild(visSheet);
  const showOnly = i => { visSheet.textContent = `[data-cell]{display:none}[data-cell]:nth-child(${i + 1}){display:block}`; };
  showOnly(-1);

  // Transition: only the content dissolves into the background (out, swap, in). The close button
  // and bottom bars sit beneath it in their own layer and never blink.
  // Looked up on every call: the runtime may replace the root element after boot.
  let fadeAnim = null, swapTimer = null;
  const content = () => document.getElementById('dc-root');
  const fadeLevel = () => { const c = content(); return c ? 1 - +getComputedStyle(c).opacity : 0; };
  function fadeTo(o, ms, easing) {
    const fade = content();
    if (!fade) return;
    const from = 1 - fadeLevel();
    if (fadeAnim) fadeAnim.cancel();
    fade.style.opacity = 1 - o;
    fadeAnim = ms > 0 ? fade.animate([{ opacity: from }, { opacity: 1 - o }], { duration: ms, easing }) : null;
  }

  const pos = () => (target === null ? idx : target);

  function go(i) {
    if (i !== demoIdx) i = Math.max(0, Math.min(tutLast, i));
    if (i === pos()) { if (target === null) fadeTo(0, FADE_IN, 'ease-out'); return; }
    target = i;
    const ms = Math.round(FADE_OUT * (1 - fadeLevel()));
    fadeTo(1, ms, 'ease-in');
    clearTimeout(swapTimer);
    swapTimer = setTimeout(swap, ms);
  }

  function swap() {
    idx = target; target = null;
    root.toggleAttribute('data-demo', inDemo());
    fit();
    showOnly(idx);
    syncChrome();
    if (inDemo()) { if (location.hash !== '#demo') { history.pushState(null, '', '#demo'); demoPushed = true; } try { localStorage.setItem('flame.seenDemo', '1'); } catch (e) {} }
    else { demoPushed = false; history.replaceState(null, '', idx ? '#' + idx : location.pathname + location.search); scrollTo(0, 0); }
    fadeTo(0, FADE_IN, 'ease-out');
  }

  // Buttons from the design carry no handlers — wire them by label.
  const ACTIONS = {
    'Back': () => go(pos() - 1),
    'Next': () => go(pos() + 1),
    'Start Tutorial': () => go(1),
    'Skip': () => go(tutLast),
    'Close': () => go(tutLast),
    'Repeat Tutorial': () => go(0),
    'Repeat tutorial': () => go(0),
    'Start Demo': () => { if (demoIdx >= 0) go(demoIdx); else window.open(FLAME_URL, '_blank', 'noopener'); },
    'Go to Minting': () => { if (demoIdx >= 0) go(demoIdx); else window.open(FLAME_URL, '_blank', 'noopener'); },
  };

  let suppressUntil = 0;
  document.addEventListener('click', e => {
    if (performance.now() < suppressUntil) { e.preventDefault(); e.stopPropagation(); return; }
    const b = e.target.closest && e.target.closest('button');
    if (!b) return;
    // The playground's own close button: step back in history when we pushed the demo entry.
    if (b.getAttribute('aria-label') === 'Close demo') { if (demoPushed) history.back(); else go(tutLast); return; }
    // "Repeat tutorial" on the playground's Me tab: leave the demo (dropping its history entry) and start over.
    if (b.getAttribute('aria-label') === 'Repeat tutorial' && inDemo()) { if (demoPushed) { restartAfterBack = true; history.back(); } else go(0); return; }
    if (b.closest('[data-screen-label^="M "]')) return;
    const act = ACTIONS[b.getAttribute('aria-label') || b.textContent.trim()];
    if (act) act();
  }, true);

  addEventListener('keydown', e => {
    if (inDemo()) return;
    if (e.key === 'ArrowRight') go(pos() + 1);
    else if (e.key === 'ArrowLeft') go(pos() - 1);
  });

  // Swipe: nothing moves; the content fades with the finger, release switches.
  let p = null;
  addEventListener('pointerdown', e => {
    if (!slides.length || inDemo() || !e.isPrimary || target !== null || (e.target.closest && e.target.closest('[data-noswipe]'))) return;
    p = { id: e.pointerId, x: e.clientX, y: e.clientY, t: performance.now(), axis: null, dx: 0 };
  });
  addEventListener('pointermove', e => {
    if (!p || e.pointerId !== p.id) return;
    const dx = e.clientX - p.x, dy = e.clientY - p.y;
    if (!p.axis) {
      if (Math.hypot(dx, dy) < 8) return;
      p.axis = Math.abs(dx) > Math.abs(dy) ? 'x' : 'y';
    }
    if (p.axis !== 'x') return;
    const edge = (idx === 0 && dx > 0) || (idx === tutLast && dx < 0);
    p.dx = edge ? dx / 3 : dx;
    fadeTo(Math.min(0.6, Math.abs(p.dx) / W * 0.9), 0);
  });
  const end = e => {
    if (!p || e.pointerId !== p.id) return;
    const q = p; p = null;
    if (q.axis !== 'x') return;
    if (Math.abs(q.dx) > 8) suppressUntil = performance.now() + 350;
    const v = q.dx / Math.max(1, performance.now() - q.t);
    if (q.dx < -W * 0.18 || v < -0.45) go(idx + 1);
    else if (q.dx > W * 0.18 || v > 0.45) go(idx - 1);
    else fadeTo(0, 180, 'ease-out');
  };
  addEventListener('pointerup', end);
  addEventListener('pointercancel', end);

  // Scroll hint in the playground: a soft fade above the tab bar while there is more below.
  const scroller = () => document.querySelector('[data-screen-label^="M "] [data-noscroll]');
  function scrollHints() {
    const sc = inDemo() ? scroller() : null;
    root.toggleAttribute('data-mp-more', !!(sc && sc.scrollHeight - sc.clientHeight - sc.scrollTop > 4));
  }
  setInterval(scrollHints, 300);
  // …and right away whenever the screen changes (switching tabs, opening More options), not on the next tick
  let hintRaf = 0;
  const kickHints = () => { if (!hintRaf) hintRaf = requestAnimationFrame(() => { hintRaf = 0; scrollHints(); }); };
  new MutationObserver(kickHints).observe(document.body, { childList: true, subtree: true });
  addEventListener('resize', kickHints);

  // Nothing in the tutorial may scroll (focus/scrollIntoView would shift the screens).
  // The playground scrolls inside itself, and iOS may shift the page for its keyboard.
  addEventListener('scroll', e => {
    if (inDemo()) { if (e.target === scroller()) scrollHints(); return; }
    const t = e.target === document ? document.scrollingElement : e.target;
    if (t && (t.scrollLeft || t.scrollTop)) { t.scrollLeft = 0; t.scrollTop = 0; }
  }, true);
  const fromHash = () => (location.hash === '#demo' && demoIdx >= 0 ? demoIdx : parseInt(location.hash.slice(1), 10) || 0);
  let restartAfterBack = false;
  addEventListener('hashchange', () => { go(restartAfterBack ? 0 : fromHash()); restartAfterBack = false; });
  addEventListener('resize', fit);
  addEventListener('orientationchange', () => setTimeout(fit, 250));
  fit();

  (function wait() {
    const s = document.querySelectorAll('[data-screen-label]');
    if (s.length < 11) return requestAnimationFrame(wait);
    slides = [...s];
    demoIdx = slides.findIndex(el => /^M /.test(el.dataset.screenLabel || ''));
    tutLast = (demoIdx >= 0 ? slides.filter((_, i) => i !== demoIdx) : slides).length - 1;
    buildChrome();
    idx = fromHash();
    // Whoever has opened the playground once skips the tutorial on later visits; "Repeat tutorial" still brings it back.
    let seen = false; try { seen = localStorage.getItem('flame.seenDemo') === '1'; } catch (e) {}
    if (!location.hash && seen && demoIdx >= 0) { idx = demoIdx; history.replaceState(null, '', '#demo'); }
    if (idx !== demoIdx) idx = Math.max(0, Math.min(tutLast, idx));
    root.toggleAttribute('data-demo', inDemo());
    fit();
    showOnly(idx);
    syncChrome();
  })();

  // Close button and bottom bars live in a fixed layer beneath the content, so they never move
  // and hints from the content can overlap them. The per-slide originals stay in the layout
  // (hidden) so content keeps its position.
  function buildChrome() {
    const layer = document.createElement('div'); layer.id = 'chrome';
    const frame = document.createElement('div'); frame.className = 'frame';
    layer.appendChild(frame);
    const bottomOf = sl => sl.children[sl.children.length - 2].lastElementChild;
    rows = [bottomOf(slides[0]), bottomOf(slides[1]), bottomOf(slides[tutLast])].map(src => {
      const row = src.cloneNode(true); row.className = 'row'; row.style.visibility = '';
      frame.appendChild(row); return row;
    });
    const dotRow = [...rows[1].children].find(el => el.tagName === 'DIV');  // the page dots in the Back · dots · Next row
    dots = dotRow ? [...dotRow.children] : [];
    const close = slides[1].querySelector('button[aria-label="Close"]');
    if (close) {
      closeBtn = close.cloneNode(true);
      closeBtn.classList.add('close');
      frame.appendChild(closeBtn);
    }
    document.body.insertBefore(layer, document.body.firstChild);
  }
  function syncChrome() {
    if (!rows.length) return;
    const which = inDemo() ? -1 : idx === 0 ? 0 : idx === tutLast ? 2 : 1;
    rows.forEach((r, i) => r.classList.toggle('on', i === which));
    if (closeBtn) closeBtn.classList.toggle('on', idx > 0 && !inDemo());
    dots.forEach((d, i) => {
      const on = i === idx - 1;
      d.style.width = on ? '14px' : '5px';
      d.style.background = on ? '#ffffff' : 'rgba(255,255,255,0.28)';
    });
  }
})();
