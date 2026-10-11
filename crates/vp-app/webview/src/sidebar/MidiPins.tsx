import { For } from 'solid-js';
import { pins, pinsReady, setPin, setPinColor } from './midi-pin-store';
import { sidebar } from './store';
import { isSubLane, laneAddressKey } from './lane';
import { sendIpc } from './ipc';
import { openContextMenu, type ContextMenuItem } from './ContextMenu';
import type { LaneTarget } from '../../midi-navigation';

export function selectPinnedLane(pin: LaneTarget): void {
  const exists = sidebar.lanes_by_repo[pin.path]?.some(l => laneAddressKey(l) === pin.address);
  if (!exists) return; // Keep the saved pin, but never select a different lane in its place.
  if (!sidebar.processes.find(p => p.path === pin.path)?.expanded) {
    sendIpc({ t: 'process:toggle', path: pin.path, expanded: true });
  }
  sendIpc({ t: 'lane:select', ...pin });
}

function activeTarget(): LaneTarget | null {
  for (const [path, lanes] of Object.entries(sidebar.lanes_by_repo)) {
    if (lanes.some(l => laneAddressKey(l) === sidebar.active_lane_address)) {
      return {path, address: sidebar.active_lane_address!};
    }
  }
  return null;
}

/** LPD8 の物理配置どおり: 上段 = pad 5〜8（ピン #5〜#8）、下段 = pad 1〜4（ピン #1〜#4）。 */
export const PIN_GRID_ORDER = [4,5,6,7,0,1,2,3];

export function MidiPins() {
  return <div class="vp-midi-pins" aria-label="LPD8 Lane ピン">
    <For each={PIN_GRID_ORDER}>{slot => {
      const pin = () => pins().slots[slot];
      const available = () => !!pin() && sidebar.lanes_by_repo[pin()!.path]?.some(l => laneAddressKey(l) === pin()!.address);
      const label = () => {
        const p = pin();
        if (!p) return '+';
        const lane = sidebar.lanes_by_repo[p.path]?.find(l => laneAddressKey(l) === p.address);
        return lane && !isSubLane(lane)
          ? sidebar.processes.find(proc => proc.path === p.path)?.name ?? p.address
          : p.address.split('/').at(-1);
      };
      let colorInput!: HTMLInputElement;
      return <div class="vp-midi-pin" style={{'--pin-color':pins().colors[slot]}}>
        <button type="button" disabled={!pinsReady()}
          classList={{selected:!!pin() && sidebar.active_lane_address === pin()!.address, unavailable:!!pin() && !available()}}
          title={pin() ? `${pin()!.address}${available() ? '' : '（現在利用できません）'} — 右クリックで設定` : `#${slot+1} に現在の Lane を固定`}
          onClick={() => { const p = pin(); if (p) selectPinnedLane(p); else { const a = activeTarget(); if (a) setPin(slot,a); } }}
          onContextMenu={e => {
            e.preventDefault();
            const items: ContextMenuItem[] = [{label:'色を変更',icon:'ph:palette',onSelect:() => colorInput.click()}];
            const a = activeTarget();
            if (a) items.unshift({label:'現在の Lane を固定',icon:'ph:push-pin',onSelect:() => setPin(slot,a)});
            if (pin()) items.push({label:'固定を解除',icon:'ph:x',onSelect:() => setPin(slot,null)});
            openContextMenu(`LPD8 #${slot+1}`,items,e.clientX,e.clientY);
          }}>
          <span>#{slot+1}</span>
          <span class="vp-midi-pin-label">{label()}</span>
        </button>
        <input ref={colorInput} class="vp-midi-color" type="color" aria-label={`LPD8 #${slot+1} の色`}
          value={pins().colors[slot]} onChange={e => setPinColor(slot,e.currentTarget.value)} />
      </div>;
    }}</For>
  </div>;
}

export const MIDI_PINS_CSS = `
.vp-midi-pins{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:2px 4px;padding:0 12px 8px;}
.vp-midi-pin{min-width:0;position:relative;}
.vp-midi-pin button{width:100%;display:flex;align-items:center;gap:4px;border:0;border-bottom:2px solid var(--pin-color);background:transparent;color:var(--color-text-secondary);padding:4px 2px;cursor:pointer;font:inherit;font-size:11px;}
.vp-midi-pin button.selected{color:var(--pin-color);background:var(--color-surface-bg-emphasis);}
.vp-midi-pin button.unavailable{opacity:.45;}
.vp-midi-pin-label{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-midi-color{position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;}
`;
