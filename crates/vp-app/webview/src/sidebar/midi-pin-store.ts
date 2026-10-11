import { createSignal } from 'solid-js';
import { assignPin, defaultPins, restorePins, type PinSettings } from '../../midi-pins';
import type { LaneTarget } from '../../midi-navigation';
import { sendIpc } from './ipc';

const [pins, setPins] = createSignal<PinSettings>(defaultPins());
const [pinsReady, setPinsReady] = createSignal(false);
export { pins, pinsReady };

export function restoreMidiPins(value: unknown): void {
  setPins(restorePins(value));
  setPinsReady(true);
}

function persist(next: PinSettings): void {
  if (!pinsReady()) return;
  setPins(next);
  sendIpc({ t: 'midi:navigation', settings: JSON.stringify(next) });
}

export function setPin(slot: number, target: LaneTarget | null): void {
  persist(assignPin(pins(), slot, target));
}

export function setPinColor(slot: number, color: string): void {
  if (!Number.isInteger(slot) || slot < 0 || slot >= 4 || !/^#[0-9a-f]{6}$/i.test(color)) return;
  const next = { slots: [...pins().slots], colors: [...pins().colors] };
  next.colors[slot] = color;
  persist(next);
}
