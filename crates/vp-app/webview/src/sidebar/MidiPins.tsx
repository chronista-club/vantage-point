import { For, Show } from 'solid-js';
import { CreoIcon } from '@chronista-club/creo-ui-icons-web';
import { pins, pinsReady, setPin, setPinColor } from './midi-pin-store';
import { sidebar } from './store';
import { laneAddressKey } from './lane';
import { laneDisplayState } from './lane-progress';
import { openContextMenu, type ContextMenuItem } from './ContextMenu';
import { pinLabel, selectLaneByNumber } from './actions/handlers';
import type { LaneTarget } from '../../midi-navigation';

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

/**
 * ピン帯。ピン #N が sidebar の #N・⌘N・LPD8 pad N の出どころ（doc 81「番号はピンが出どころ」）。
 * 空枠 click = 今選んでいる lane を登録 / 登録済み枠 click = ⌘N と同じ関数で開く / hover の ✕ = 解除。
 */
export function MidiPins() {
  return <div class="vp-midi-pins" aria-label="LPD8 Lane ピン">
    <For each={PIN_GRID_ORDER}>{slot => {
      const number = slot + 1;
      const pin = () => pins().slots[slot];
      const available = () => !!pin() && sidebar.lanes_by_repo[pin()!.path]?.some(l => laneAddressKey(l) === pin()!.address);
      const requesting = () => {
        const lane = pin() && sidebar.lanes_by_repo[pin()!.path]?.find(l => laneAddressKey(l) === pin()!.address);
        return !!lane && laneDisplayState(lane) === 'requesting';
      };
      const register = () => { const a = activeTarget(); if (a) setPin(slot, a); };
      let colorInput!: HTMLInputElement;
      return <div class="vp-midi-pin" style={{'--pin-color':pins().colors[slot]}}>
        <button type="button" disabled={!pinsReady()}
          classList={{selected:!!pin() && sidebar.active_lane_address === pin()!.address, unavailable:!!pin() && !available(), requesting:requesting()}}
          title={pin()
            ? `#${number}（⌘${number} / pad ${number}）${pin()!.address}${available() ? '' : '（現在利用できません）'} — 右クリックで設定`
            : `#${number} に今選んでいる Lane を登録`}
          onClick={() => pin() ? selectLaneByNumber(number) : register()}
          onContextMenu={e => {
            e.preventDefault();
            const items: ContextMenuItem[] = [{label:'色を変更',icon:'ph:palette',onSelect:() => colorInput.click()}];
            if (activeTarget()) items.unshift({label:'今選んでいる Lane を登録',icon:'ph:push-pin',onSelect:register});
            if (pin()) items.push({label:'登録を解除',icon:'ph:x',onSelect:() => setPin(slot,null)});
            openContextMenu(`LPD8 #${number}`,items,e.clientX,e.clientY);
          }}>
          <span>#{number}</span>
          <span class="vp-midi-pin-label">{pin() ? pinLabel(pin()!) : '+'}</span>
        </button>
        <Show when={pin() && pinsReady()}>
          <button type="button" class="vp-midi-unpin" title={`#${number} の登録を解除`} aria-label={`#${number} の登録を解除`}
            onClick={e => { e.stopPropagation(); setPin(slot,null); }}>
            <CreoIcon name="ph:x" size={10} />
          </button>
        </Show>
        <input ref={colorInput} class="vp-midi-color" type="color" aria-label={`LPD8 #${number} の色`}
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
.vp-midi-pin button.requesting{color:var(--sb-conn-hitl,#f28dd3);border-color:var(--sb-conn-hitl,#f28dd3);}
.vp-midi-pin-label{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-midi-unpin{position:absolute;top:1px;right:0;display:flex;align-items:center;justify-content:center;width:14px;height:14px;padding:0;border:0;border-radius:3px;background:var(--color-surface-bg-emphasis);color:var(--color-text-secondary);cursor:pointer;opacity:0;transition:opacity .1s;}
.vp-midi-pin:hover .vp-midi-unpin,.vp-midi-unpin:focus-visible{opacity:1;}
.vp-midi-unpin:hover{color:var(--color-text-primary);}
.vp-midi-color{position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;}
`;
