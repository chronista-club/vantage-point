import type {FleetPayload} from '../../fleet';
import {selectLaneByNumber} from './actions/handlers';

/** The daemon validates the VP setup's channel/CC contract before emitting buttons. */
export function createRotoNavigation() {
  const held = new Set<number>();
  window.addEventListener('vp:midi-target', () => held.clear());
  window.addEventListener('vp:midi-unavailable', event => {
    if ((event as CustomEvent<{port_name?: string}>).detail?.port_name?.toLowerCase().includes('roto')) held.clear();
  });
  function handle(payload: FleetPayload): boolean {
    if (!payload.port_name?.toLowerCase().includes('roto')) return false;
    const e = payload.event;
    // ROTO knobs/touch/transport have no VP action; do not fall through to gallery.
    if (!e || e.type !== 'button' || !Number.isInteger(e.index) || e.index < 0 || e.index >= 8 || typeof e.pressed !== 'boolean') return true;
    if (e.pressed === held.has(e.index)) return true;
    if (e.pressed) held.add(e.index); else held.delete(e.index);
    if (e.index === 7) {
      window.dispatchEvent(new CustomEvent('vp:roto-ptt', {detail:{pressed:e.pressed}}));
    } else if (e.pressed) {
      // Same pin SSOT and jump action as LPD8 pads / ⌘1…⌘7.
      selectLaneByNumber(e.index + 1);
    }
    return true;
  }
  return {handle};
}
