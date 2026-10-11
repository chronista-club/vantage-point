import type { LaneTarget } from './midi-navigation';
import type { FleetPayload } from './fleet';
import type { FleetFeedback } from './fleet';

export interface NavigationFeedback { enabled: boolean; pads: {index:number;filled:boolean;color:number[]}[] }
export function navigationFeedback(gallery: FleetFeedback, navigation: NavigationFeedback): FleetFeedback | null {
  return navigation.enabled ? {...gallery,pads:[...gallery.pads.filter(p => p.index < 4),...navigation.pads]} : null;
}

export interface PinSettings { slots: (LaneTarget | null)[]; colors: string[] }
export const defaultPins = (): PinSettings => ({
  slots: [null, null, null, null],
  colors: ['#44c2b5', '#639af5', '#b78aee', '#f0b45a'],
});

export function assignPin(state: PinSettings, slot: number, target: LaneTarget | null): PinSettings {
  if (!Number.isInteger(slot) || slot < 0 || slot >= 4) return state;
  return { colors: [...state.colors], slots: state.slots.map((pin, i) =>
    i === slot ? target : target && pin?.address === target.address ? null : pin) };
}

export function restorePins(value: unknown): PinSettings {
  const v = value as PinSettings | null;
  if (!v || !Array.isArray(v.slots) || v.slots.length !== 4 ||
    !Array.isArray(v.colors) || v.colors.length !== 4 ||
    !v.colors.every(c => typeof c === 'string' && /^#[0-9a-f]{6}$/i.test(c)) ||
    !v.slots.every(p => p === null || (typeof p?.path === 'string' && p.path.length > 0 &&
      typeof p?.address === 'string' && p.address.length > 0))) return defaultPins();
  return { slots: v.slots.map(p => p ? {path:p.path,address:p.address} : null), colors:[...v.colors] };
}

/** Physical top row is pads 5–8 (zero-based indices 4–7). Missing lanes are dark. */
export function pinFeedback(state: PinSettings, active: string | null, available: Set<string>) {
  return state.slots.map((pin, i) => {
    const present = !!pin && available.has(pin.address);
    const gain = !present ? 0 : pin.address === active ? 1 : 0.25;
    const color = [1, 3, 5].map(offset => Math.round(parseInt(state.colors[i].slice(offset, offset + 2), 16) * gain));
    return { index: i + 4, filled: present, color };
  });
}

export function pinPadHandler(settings: () => PinSettings, select: (target: LaneTarget) => void) {
  const held = new Set<number>();
  return (payload: FleetPayload): boolean => {
    const e = payload.event;
    if (!payload.port_name?.includes('LPD8') || e?.type !== 'pad' ||
      !Number.isInteger(e.index) || e.index < 4 || e.index > 7) return false;
    if (!e.velocity || e.velocity < 0) { held.delete(e.index); return true; }
    if (!held.has(e.index)) {
      held.add(e.index);
      const pin = settings().slots[e.index - 4];
      if (pin) select(pin);
    }
    return true;
  };
}
