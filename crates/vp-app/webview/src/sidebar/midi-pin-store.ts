import { createSignal } from 'solid-js';
import { assignPin, defaultPins, restorePins, withPinColor, type PinSettings } from '../../midi-pins';
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
  const next = withPinColor(pins(), slot, color);
  if (next !== pins()) persist(next);
}
