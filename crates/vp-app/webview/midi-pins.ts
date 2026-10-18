import type { LaneTarget } from './midi-navigation';
import type { FleetPayload } from './fleet';
import type { FleetFeedback } from './fleet';

export interface NavigationFeedback { enabled: boolean; pads: {index:number;filled:boolean;color:number[]}[] }
export function navigationFeedback(gallery: FleetFeedback, navigation: NavigationFeedback): FleetFeedback | null {
  // LPD8 の pad 1〜8 はすべてピン。gallery の Scene slot 点灯は投影しない。
  return navigation.enabled ? {...gallery,pads:[...navigation.pads]} : null;
}

/** LPD8 の pad 1〜8（内部 index 0〜7）がそのままピン #1〜#8。 */
export const PIN_COUNT = 8;
/** 先頭 4 色は 4 ピン時代の既定。後ろ 4 色は物理の上下・左右で隣と見分けやすい色相に置く。 */
export const DEFAULT_PIN_COLORS: readonly string[] = [
  '#44c2b5', '#639af5', '#b78aee', '#f0b45a',
  '#c4d655', '#ef7b72', '#6fcf7e', '#e47ac9',
];
/** 未選択ピンの LED 明るさ（設定色に掛ける倍率）。旧 session には無い。 */
export const DEFAULT_DIM_GAIN = 0.25;

export interface PinSettings { slots: (LaneTarget | null)[]; colors: string[]; dimGain: number }
export const defaultPins = (): PinSettings => ({
  slots: Array(PIN_COUNT).fill(null),
  colors: [...DEFAULT_PIN_COLORS],
  dimGain: DEFAULT_DIM_GAIN,
});

const isSlot = (slot: number) => Number.isInteger(slot) && slot >= 0 && slot < PIN_COUNT;
export const isPinColor = (color: unknown): color is string => typeof color === 'string' && /^#[0-9a-f]{6}$/i.test(color);
export const isDimGain = (gain: unknown): gain is number => typeof gain === 'number' && Number.isFinite(gain) && gain >= 0 && gain <= 1;

export function assignPin(state: PinSettings, slot: number, target: LaneTarget | null): PinSettings {
  if (!isSlot(slot)) return state;
  return { ...state, colors: [...state.colors], slots: state.slots.map((pin, i) =>
    i === slot ? target : target && pin?.address === target.address ? null : pin) };
}

export function withPinColor(state: PinSettings, slot: number, color: string): PinSettings {
  if (!isSlot(slot) || !isPinColor(color)) return state;
  return { ...state, slots: [...state.slots], colors: state.colors.map((c, i) => i === slot ? color : c) };
}

export function withDimGain(state: PinSettings, dimGain: number): PinSettings {
  return isDimGain(dimGain) ? { ...state, slots: [...state.slots], colors: [...state.colors], dimGain } : state;
}

/** 旧 4 ピンの session は後ろ 4 枠を空 + 既定色で埋めて読む。dimGain だけ壊れていてもピンは残す。 */
export function restorePins(value: unknown): PinSettings {
  const v = value as { slots?: unknown; colors?: unknown; dimGain?: unknown } | null;
  const slots = v?.slots, colors = v?.colors;
  if (!Array.isArray(slots) || !Array.isArray(colors) || slots.length !== colors.length ||
    (slots.length !== 4 && slots.length !== PIN_COUNT) ||
    !colors.every(isPinColor) ||
    !slots.every(p => p === null || (typeof p?.path === 'string' && p.path.length > 0 &&
      typeof p?.address === 'string' && p.address.length > 0))) return defaultPins();
  return {
    slots: Array.from({length: PIN_COUNT}, (_, i) => slots[i] ? {path: slots[i].path, address: slots[i].address} : null),
    colors: Array.from({length: PIN_COUNT}, (_, i) => colors[i] ?? DEFAULT_PIN_COLORS[i]),
    dimGain: isDimGain(v?.dimGain) ? v.dimGain : DEFAULT_DIM_GAIN,
  };
}

/**
 * lane の address → 登録しているピン番号（1〜8）。登録していなければ null。
 *
 * ⚠️ **番号の出どころはこの 2 関数だけ**（doc 81「番号はピンが出どころ」）: sidebar の `#N`、⌘N、
 * LPD8 pad N の三者がここを通る。並び順など別の物差しで数えると「見えている番号と飛び先が違う」になる。
 */
export function pinNumberOf(state: PinSettings, address: string): number | null {
  const i = state.slots.findIndex(pin => pin?.address === address);
  return i < 0 ? null : i + 1;
}

/** ピン番号（1〜8）→ 登録している lane。範囲外・未登録は null。 */
export function pinTargetOf(state: PinSettings, number: number): LaneTarget | null {
  return isSlot(number - 1) ? state.slots[number - 1] ?? null : null;
}

/** Pads 1–8 (zero-based indices 0–7). Missing lanes are dark. */
export function pinFeedback(state: PinSettings, active: string | null, available: Set<string>) {
  return state.slots.map((pin, i) => {
    const present = !!pin && available.has(pin.address);
    const gain = !present ? 0 : pin.address === active ? 1 : state.dimGain;
    const color = [1, 3, 5].map(offset => Math.round(parseInt(state.colors[i].slice(offset, offset + 2), 16) * gain));
    return { index: i, filled: present, color };
  });
}

/** 登録済み pad の長押し判定。空枠は押した瞬間に登録する。 */
export const PIN_HOLD_MS = 600;
export type PinNotice = 'registered' | 'unavailable';
interface PinPadActions {
  settings: () => PinSettings;
  active: () => LaneTarget | null;
  assign: (slot: number, target: LaneTarget) => void;
  notify: (slot: number, notice: PinNotice) => void;
}

/** 短押しの宛先は ⌘N と同じ select に任せ、各 pad の押下を独立して扱う。 */
export function pinPadHandler(select: (number: number) => void, actions: PinPadActions) {
  const held = new Map<number, {open: boolean; timer?: ReturnType<typeof setTimeout>}>();
  const register = (slot: number) => {
    const active = actions.active();
    if (!active) { actions.notify(slot, 'unavailable'); return; }
    if (actions.settings().slots[slot]?.address === active.address) return;
    actions.assign(slot, active);
    actions.notify(slot, 'registered');
  };
  const handle = (payload: FleetPayload): boolean => {
    const e = payload.event;
    if (!payload.port_name?.includes('LPD8') || e?.type !== 'pad' ||
      !Number.isInteger(e.index) || !isSlot(e.index)) return false;
    const press = held.get(e.index);
    if (!e.velocity || e.velocity < 0) {
      if (press) {
        clearTimeout(press.timer);
        held.delete(e.index);
        if (press.open) select(e.index + 1);
      }
      return true;
    }
    if (press) return true;
    const next = {open: !!actions.settings().slots[e.index], timer: undefined as ReturnType<typeof setTimeout> | undefined};
    held.set(e.index, next);
    if (!next.open) register(e.index);
    else next.timer = setTimeout(() => {
      next.open = false;
      register(e.index);
    }, PIN_HOLD_MS);
    return true;
  };
  return Object.assign(handle, {cancel: () => {
    for (const press of held.values()) clearTimeout(press.timer);
    held.clear();
  }});
}

/** 一時色は通常の feedback に重ねる。null で最新の通常色へ戻す。 */
export function pinBlinker(update: (slot: number, color: number[] | null) => void) {
  const timers = new Map<number, ReturnType<typeof setTimeout>[]>();
  const clear = (slot: number) => {
    for (const timer of timers.get(slot) ?? []) clearTimeout(timer);
    timers.delete(slot);
  };
  return {
    flash(slot: number, color: number[]) {
      clear(slot);
      update(slot, color);
      timers.set(slot, [150,300,450,600].map((delay, i) => setTimeout(() => {
        update(slot, i === 3 ? null : i % 2 === 0 ? [0,0,0] : color);
        if (i === 3) timers.delete(slot);
      }, delay)));
    },
    cancel() {
      for (const slot of timers.keys()) { clear(slot); update(slot, null); }
    },
  };
}
