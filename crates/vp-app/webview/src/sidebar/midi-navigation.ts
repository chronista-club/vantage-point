import { createRotoNavigation } from './roto-navigation';
import { createEffect, createRoot, createSignal } from 'solid-js';
import { currentLanes, laneKnobHandler } from '../../midi-navigation';
import { pinFeedback, pinPadHandler } from '../../midi-pins';
import type { FleetPayload } from '../../fleet';
import { sidebar } from './store';
import { sendIpc } from './ipc';
import { pins, pinsReady, restoreMidiPins } from './midi-pin-store';
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
// pad N = ⌘N: 宛先は ⌘N と同じ関数で引く（番号の出どころを 2 つ作らない）。
let selectPin = pinPadHandler(selectLaneByNumber);
const [target, setTarget] = createSignal(document.documentElement.dataset.midiTarget === 'true');
window.addEventListener('vp:midi-target', e => {
  selectLane = createHandler();
  selectPin = pinPadHandler(selectLaneByNumber);
  setTarget((e as CustomEvent<boolean>).detail);
});

window.addEventListener('vp:midi-settings', e => restoreMidiPins((e as CustomEvent).detail));
const restored = (window as unknown as {__vpMidiSettings?:unknown}).__vpMidiSettings;
if (restored !== undefined) restoreMidiPins(restored);

const roto = createRoot(() => createRotoNavigation());

// This module belongs to the sidebar bundle, so it reads the same store as CURRENTs.
window.addEventListener('vp:midi-navigation', event => {
  const payload = (event as CustomEvent<FleetPayload>).detail;
  if (!target()) return;
  if (roto.handle(payload) || selectLane(payload) || selectPin(payload)) event.preventDefault();
});

// The main bundle owns the single fleet feedback stream and combines these upper-pad
// colors with the gallery's remaining controls. Retain the latest snapshot across load order.
createRoot(() => createEffect(() => {
  const pads = pinFeedback(pins(), sidebar.active_lane_address ?? null,
    new Set(currentLanes(sidebar).map(l => l.address)));
  const enabled = target() && pinsReady();
  const rotoSlots = roto.feedback();
  (window as unknown as {__vpMidiPads?:unknown}).__vpMidiPads = {enabled,pads,roto:rotoSlots};
  window.dispatchEvent(new CustomEvent('vp:midi-pads', {detail:{enabled,pads,roto:rotoSlots}}));
}));
