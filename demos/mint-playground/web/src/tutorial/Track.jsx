import { useCallback, useRef } from 'react';

/**
 * A drag along a slider track: `onFraction` gets 0–1 of the track's width while the pointer is
 * down, from the first touch on. Spread the result on the track element.
 */
export function useTrack(onFraction) {
  const ref = useRef(null);
  const onPointerDown = useCallback(
    (ev) => {
      const el = ref.current;
      if (!el) return;
      const update = (e) => {
        const r = el.getBoundingClientRect();
        onFraction(Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)));
      };
      update(ev);
      const up = () => {
        window.removeEventListener('pointermove', update);
        window.removeEventListener('pointerup', up);
      };
      window.addEventListener('pointermove', update);
      window.addEventListener('pointerup', up);
    },
    [onFraction],
  );
  return { ref, onPointerDown };
}

/** The slider itself: a thin bar filled up to `pct` percent and a white knob there. */
export function Track({ track, pct, height, top }) {
  const knob = 28;
  return (
    <div
      ref={track.ref}
      data-noswipe=""
      onPointerDown={track.onPointerDown}
      style={{
        position: 'relative',
        height,
        margin: height === 44 ? '2px 14px 0' : '0 14px',
        touchAction: 'none',
        cursor: 'pointer',
      }}
    >
      <div
        style={{
          position: 'absolute',
          left: -14,
          right: -14,
          top,
          height: 4,
          borderRadius: 2,
          background: 'rgba(255,255,255,0.16)',
        }}
      />
      <div
        style={{
          position: 'absolute',
          left: -14,
          top,
          height: 4,
          borderRadius: 2,
          background: '#ff3b1f',
          width: `calc(${pct}% + 14px)`,
        }}
      />
      <div
        style={{
          position: 'absolute',
          top: top - 12,
          width: knob,
          height: knob,
          marginLeft: -14,
          borderRadius: 14,
          background: '#ffffff',
          boxShadow: '0 2px 8px rgba(0,0,0,0.5)',
          left: `${pct}%`,
        }}
      />
    </div>
  );
}
