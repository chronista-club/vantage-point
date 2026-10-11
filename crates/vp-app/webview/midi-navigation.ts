import type { SidebarState } from './src/generated/SidebarState';
import type { FleetPayload } from './fleet';
import { resolveRepoOrder } from './src/sidebar/dnd';
import { isRunningProcess } from './src/sidebar/classify';
import { isSubLane, laneAddressKey } from './src/sidebar/lane';

export type LaneTarget = { path: string; address: string };
type LaneState = Pick<SidebarState, 'processes' | 'currents_order' | 'lanes_by_repo'>;

/** Same ordering and CURRENTs membership as Shell / RepoAccordion, including collapsed lanes. */
export function currentLanes(state: LaneState): LaneTarget[] {
  return resolveRepoOrder(state.processes, state.currents_order).filter(isRunningProcess).flatMap(proc => {
    const lanes = state.lanes_by_repo[proc.path] ?? [];
    const root = lanes.find(l => !isSubLane(l));
    return [...(root ? [root] : []), ...lanes.filter(isSubLane)]
      .map(l => ({ path: proc.path, address: laneAddressKey(l) }));
  });
}

/** Recover the 7-bit value before dividing into equal intervals (wire values are f32). */
export function laneIndex(value: number, count: number): number | null {
  if (!Number.isFinite(value) || count <= 0) return null;
  const midi = Math.round(Math.max(0, Math.min(1, value)) * 127);
  return midi === 127 ? count - 1 : Math.min(count - 1, Math.floor(midi * count / 128));
}

export function laneKnobHandler(targets: () => LaneTarget[], select: (target: LaneTarget) => void) {
  let last: string | null = null;
  return (payload: FleetPayload): boolean => {
    const e = payload.event;
    if (!payload.port_name?.includes('LPD8') || e?.type !== 'knob' || e.index !== 0) return false;
    const lanes = targets();
    const index = laneIndex(e.value ?? NaN, lanes.length);
    if (index === null) { last = null; return true; }
    const lane = lanes[index];
    if (lane.address !== last) { last = lane.address; select(lane); }
    return true;
  };
}
