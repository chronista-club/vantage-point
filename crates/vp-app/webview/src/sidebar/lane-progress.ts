import type { LaneInfo } from '../generated/LaneInfo';
import { isLaneAlive, rootModeOf } from './lane';
import type { AgentActivity } from '../../session-activity-bridge';

export type LaneDisplayState = 'requesting' | 'working' | 'waiting' | 'unread' | 'stopped';
export const LANE_STATE_LABEL: Record<LaneDisplayState, string> = {
  requesting: '要求中', working: '作業中', waiting: '指示待ち', unread: '完了・未読', stopped: '停止',
};
export const LANE_STATE_GLYPH: Record<LaneDisplayState, string> = {
  requesting: '◆', working: '◌', waiting: '●', unread: '✓', stopped: '☾',
};

/** 行の代表 session は registry の root。focused の会話と混同しない。 */
export function rootProgress(lane: LaneInfo) {
  return lane.sessions?.sessions.find(s => s.key === lane.sessions?.root)?.progress;
}

/** 未読は wire inbox の事実。要求検出は chat に限定し、TUI の入力待ちを要求と推測しない。 */
export function laneDisplayState(lane: LaneInfo, unread = 0, live?: AgentActivity): LaneDisplayState {
  const progress = rootModeOf(lane) === 'gui' ? rootProgress(lane) : undefined;
  if (!isLaneAlive(lane) || progress?.phase === 'stopped' || (!progress && live === 'error')) return 'stopped';
  if (rootModeOf(lane) === 'gui' && progress?.phase === 'requesting') return 'requesting';
  if (progress?.phase === 'working') return 'working';
  if (!progress && (live === 'working' || live === 'thinking')) return 'working';
  // wire の開発フローは会話の実行状態ではない。GUI の再接続で progress が無くても推測しない。
  if (rootModeOf(lane) !== 'gui' &&
    (lane.flow_state === 'working' || lane.flow_state === 'hitl_pending' || lane.flow_state === 'stuck')) return 'working';
  if (unread > 0) return 'unread';
  return 'waiting';
}

/** 未観測は経過を捏造せず —。未来時刻は時計差として now に丸める。 */
export function activityElapsed(at: number | null | undefined, now: number): string {
  if (at == null || !Number.isFinite(at) || at <= 0) return '—';
  const minutes = Math.floor(Math.max(0, now - at) / 60_000);
  return minutes < 1 ? 'now' : minutes < 60 ? `${minutes}m` : `${Math.floor(minutes / 60)}h`;
}

export function requestingLaneCount(lanes: readonly LaneInfo[]): number {
  return lanes.filter(lane => laneDisplayState(lane) === 'requesting').length;
}
