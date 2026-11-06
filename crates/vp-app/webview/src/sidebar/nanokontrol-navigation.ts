import type { FleetPayload } from '../../fleet';
import { selectLaneByNumber } from './actions/handlers';
import { currentLanes } from '../../midi-navigation';
import { sidebar } from './store';
import { sendIpc } from './ipc';

/** Standard CC S1–S8 are buttons 0–7; TRACK left/right are 24/25. */
export function createNanoKontrolNavigation() {
  const held = new Set<number>();
  const isNanoKontrol = (port?: string) => port?.toLowerCase().includes('nanokontrol') === true;
  window.addEventListener('vp:midi-target', () => held.clear());
  window.addEventListener('vp:midi-unavailable', event => {
    if (isNanoKontrol((event as CustomEvent<{port_name?: string}>).detail?.port_name)) held.clear();
  });
  function handle(payload: FleetPayload): boolean {
    const e = payload.event;
    if (!isNanoKontrol(payload.port_name) || e?.type !== 'button' ||
      !Number.isInteger(e.index) || !((e.index >= 0 && e.index < 8) || e.index === 24 || e.index === 25)) return false;
    if (typeof e.pressed !== 'boolean' || e.pressed === held.has(e.index)) return true;
    if (e.pressed) {
      held.add(e.index);
      if (e.index < 8) {
        selectLaneByNumber(e.index + 1);
      } else {
        const lanes = currentLanes(sidebar);
        const active = lanes.findIndex(lane => lane.address === sidebar.active_lane_address);
        const step = e.index === 24 ? -1 : 1;
        const index = active < 0 ? (step > 0 ? 0 : lanes.length - 1) : active + step;
        const target = lanes[index];
        if (target) {
          if (!sidebar.processes.find(p => p.path === target.path)?.expanded) {
            sendIpc({ t: 'process:toggle', path: target.path, expanded: true });
          }
          sendIpc({ t: 'lane:select', ...target });
        }
      }
    } else {
      held.delete(e.index);
    }
    return true;
  }
  return { handle };
}
