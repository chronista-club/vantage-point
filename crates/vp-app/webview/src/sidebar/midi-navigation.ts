import { createEffect, createRoot, createSignal } from 'solid-js';
import { currentLanes, laneKnobHandler } from '../../midi-navigation';
import { pinFeedback, pinPadHandler, pinBlinker } from '../../midi-pins';
import type { FleetPayload } from '../../fleet';
import { sidebar } from './store';
import { laneAddressKey } from './lane';
import { sendIpc } from './ipc';
import { pins, pinsReady, restoreMidiPins, setPin } from './midi-pin-store';
import { selectLaneByNumber } from './actions/handlers';

const createHandler = () => laneKnobHandler(() => currentLanes(sidebar), target => {
  if (!sidebar.processes.find(p => p.path === target.path)?.expanded) {
    sendIpc({ t: 'process:toggle', path: target.path, expanded: true });
  }
  if (sidebar.active_lane_address !== target.address) {
    sendIpc({ t: 'lane:select', ...target });
  }
});

let selectLane = createHandler();
const [flashes, setFlashes] = createSignal<Record<number, number[]>>({});
const blink = pinBlinker((slot, color) => setFlashes(previous => {
  const next = {...previous};
  if (color) next[slot] = color; else delete next[slot];
  return next;
}));
const selectPin = pinPadHandler(selectLaneByNumber, {
  settings: pins,
  active: () => {
    if (sidebar.active_component || !sidebar.active_lane_address) return null;
    for (const [path, lanes] of Object.entries(sidebar.lanes_by_repo)) {
      if (lanes.some(l => laneAddressKey(l) === sidebar.active_lane_address)) {
        return {path, address: sidebar.active_lane_address};
      }
    }
    return null;
  },
  assign: setPin,
  notify: (slot, notice) => {
    const configured = pins().colors[slot];
    const color = notice === 'unavailable' ? '#ff3030' : configured === '#000000' ? '#ffffff' : configured;
    diagnosePin(slot, notice === 'registered' ? notice : sidebar.active_component ? 'component_active' :
      !sidebar.active_lane_address ? 'no_active_lane' : 'lane_not_found', color);
    blink.flash(slot, [1,3,5].map(offset => parseInt(color.slice(offset, offset + 2),16)));
  },
});
const [target, setTarget] = createSignal(document.documentElement.dataset.midiTarget === 'true');
window.addEventListener('vp:midi-target', e => {
  selectLane = createHandler();
  selectPin.cancel();
  blink.cancel();
  setTarget((e as CustomEvent<boolean>).detail);
});
window.addEventListener('pagehide', () => { selectPin.cancel(); blink.cancel(); });

window.addEventListener('vp:midi-settings', e => restoreMidiPins((e as CustomEvent).detail));
const restored = (window as unknown as {__vpMidiSettings?:unknown}).__vpMidiSettings;
if (restored !== undefined) restoreMidiPins(restored);

// entry.tsx の console bridge → 既存の t=console → Rust tracing (target=webview)。
function diagnosePin(slot: number, reason: string, color?: string): void {
  console.info('[midi:pin]', {
    pin: slot + 1, reason, target: target(), pinsReady: pinsReady(),
    active_component: sidebar.active_component ? {...sidebar.active_component} : null,
    active_lane_address: sidebar.active_lane_address ?? null,
    repo_keys: Object.keys(sidebar.lanes_by_repo),
    assigned_address: pins().slots[slot]?.address ?? null,
    ...(color ? {color} : {}),
  });
}

// This module belongs to the sidebar bundle, so it reads the same store as CURRENTs.
window.addEventListener('vp:midi-navigation', event => {
  const payload = (event as CustomEvent<FleetPayload>).detail;
  const e = payload.event;
  if (payload.port_name?.includes('LPD8') && e?.type === 'pad' && (e.velocity ?? 0) > 0 &&
    Number.isInteger(e.index) && e.index >= 0 && e.index < pins().slots.length) {
    diagnosePin(e.index, !target() ? 'target_inactive' : !pinsReady() ? 'settings_not_ready' : 'press');
  }
  if (!target()) { event.preventDefault(); return; }
  if (!pinsReady() && payload.port_name?.includes('LPD8') && payload.event?.type === 'pad') {
    event.preventDefault(); return;
  }
  if (selectLane(payload) || selectPin(payload)) event.preventDefault();
});

// The main bundle owns the single fleet feedback stream and combines these pad
// colors with the gallery's remaining controls. Retain the latest snapshot across load order.
createRoot(() => createEffect(() => {
  const pads = pinFeedback(pins(), sidebar.active_lane_address ?? null,
    new Set(currentLanes(sidebar).map(l => l.address))).map(pad =>
      flashes()[pad.index] ? {...pad, filled:true, color:flashes()[pad.index]} : pad);
  const enabled = target() && pinsReady();
  (window as unknown as {__vpMidiPads?:unknown}).__vpMidiPads = {enabled,pads};
  window.dispatchEvent(new CustomEvent('vp:midi-pads', {detail:{enabled,pads}}));
}));
