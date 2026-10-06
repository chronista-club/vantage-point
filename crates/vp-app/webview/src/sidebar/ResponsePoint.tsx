import { createSignal, onCleanup, onMount } from 'solid-js';
import type { LaneInfo } from '../generated/LaneInfo';
import { clockNow } from './activity-freshness';
import { laneAddressKey } from './lane';
import { onAgentActivity, readAgentActivity, type AgentActivity } from '../../session-activity-bridge';

type ActivityProps = { lane?: LaneInfo; session?: number; connectorClass?: string; timestamp?: number | null; compact?: boolean; disabled?: boolean };
export const ACTIVITY_LABEL: Record<AgentActivity, string> = {
  thinking: '思考中', working: '作業中', waiting: '確認待ち', completed: '応答完了・次の指示待ち', idle: '停止中・状態未取得', error: 'エラー・対応が必要',
};
export const ACTIVITY_COLOR: Record<AgentActivity, string> = {
  thinking: 'var(--sb-activity-thinking, #c3b4e8)', working: 'var(--sb-activity-working, #a7c9ec)', waiting: 'var(--sb-activity-waiting, #ef777d)', completed: 'var(--sb-activity-completed, #527ec6)', idle: 'var(--sb-activity-idle, #8c98a5)', error: 'var(--sb-activity-error, #ef777d)',
};

/** Subscribe per session, including events that arrived before the sidebar mounted. */
export function createAgentActivity(props: ActivityProps) {
  const [revision, setRevision] = createSignal(0);
  const key = () => props.session ?? props.lane?.sessions?.root ?? 1;
  const addr = () => props.lane ? laneAddressKey(props.lane) : '';
  onMount(() => {
    setRevision(v => v + 1);
    onCleanup(onAgentActivity(d => {
      if (d.lane === addr() && d.session === key()) setRevision(v => v + 1);
    }));
  });
  const live = () => { revision(); return readAgentActivity(addr(), key()); };
  const timestamp = () => {
    if (live()?.phase === 'completed') return live()!.at;
    return props.timestamp ?? props.lane?.sessions?.sessions?.find(s => s.key === key())?.last_response_at;
  };
  const phase = (): AgentActivity => {
    if (props.disabled) return 'idle';
    const current = live();
    if (current && current.phase !== 'idle') return current.phase;
    if (!current && props.connectorClass === 'conn-hitl') return 'waiting';
    if (typeof timestamp() === 'number' && Number.isFinite(timestamp()) && timestamp()! > 0) return 'completed';
    if (current) return 'idle';
    return props.connectorClass === 'conn-auto' || props.connectorClass === 'conn-run' ? 'working' : 'idle';
  };
  return { phase, timestamp };
}

/** The left-hand activity point also carries response freshness after Result. */
export function ResponsePoint(props: ActivityProps) {
  const activity = createAgentActivity(props);
  const age = () => Math.max(0, clockNow() - (activity.timestamp() ?? clockNow()));
  const freshness = () => age() < 5 * 60_000 ? 'fresh' : age() < 60 * 60_000 ? 'recent' : 'old';
  const label = () => {
    if (activity.phase() !== 'completed') return ACTIVITY_LABEL[activity.phase()];
    const minutes = Math.floor(age() / 60_000);
    const elapsed = minutes < 1 ? '1分未満' : minutes < 60 ? `${minutes}分` : minutes < 1440 ? `${Math.floor(minutes / 60)}時間` : `${Math.floor(minutes / 1440)}日`;
    return `応答完了・次の指示待ち — 最終応答から${elapsed}`;
  };
  return <span class="vp-activity-point" classList={{ 'vp-response-point': activity.phase() === 'completed', 'vp-sub-point': props.compact }}
    role="img" data-activity={activity.phase()} data-freshness={activity.phase() === 'completed' ? freshness() : undefined}
    title={props.compact ? `${props.lane?.address.name} — ${label()}` : label()} aria-label={label()}
    style={{ background: ACTIVITY_COLOR[activity.phase()], opacity: activity.phase() === 'completed' ? (freshness() === 'fresh' ? 1 : freshness() === 'recent' ? 0.8 : 0.55) : undefined }} />;
}

export const ACTIVITY_POINT_CSS = `
.vp-activity-point { display:inline-block; width:8px; height:8px; flex-shrink:0; border-radius:50%; }
.vp-activity-point[data-activity="thinking"], .vp-activity-point[data-activity="working"] { animation:vp-agent-breathe 2.8s ease-in-out infinite; }
.vp-activity-point[data-activity="idle"] { opacity:.45; }
@keyframes vp-agent-breathe { 0%,100% { opacity:.4; } 50% { opacity:1; } }
@media (prefers-reduced-motion:reduce) { .vp-activity-point { animation:none !important; } }
`;
