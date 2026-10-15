import {PIN_COUNT, type PinSettings} from './midi-pins';
import type {AgentActivity} from './session-activity-bridge';

export interface RotoLane {
  address: string; name: string; focused: number;
  sessions: {key:number; name:string}[];
  activity: AgentActivity; contextPercent: number | null;
}
export interface RotoSlot {
  index:number; name:string; color:number[]; id:string; touched?:boolean;
  status:string; steps:number; value:number; labels:string[];
}
export function sessionValue(index:number, count:number):number {
  return count <= 1 ? 0 : Math.max(0, Math.min(count-1,index)) / (count-1);
}
export function sessionIndex(value:number, count:number):number|null {
  if (!Number.isFinite(value) || count <= 0) return null;
  return Math.round(Math.max(0,Math.min(1,value)) * (count-1));
}
export function statusText(activity:AgentActivity | 'offline', percent:number|null):string {
  const label = {thinking:'Think',working:'Work',waiting:'Ask',completed:'Idle',idle:'Idle',error:'Error',offline:'Offline'}[activity];
  return `${label} ${percent == null || !Number.isFinite(percent) ? '--' : Math.round(Math.max(0, Math.min(100,percent)))}%`;
}
/** Above 16 steps ROTO has no step-label table; keep the selected name visible. */
export function parameterText(activity:AgentActivity | 'offline', percent:number|null, selected:string, count:number):string {
  const status=statusText(activity,percent);
  if(count<=16) return status;
  const [word,context]=status.split(' ');
  const short:Record<string,string>={Idle:'Id',Think:'Th',Work:'Wk',Ask:'Ask',Error:'Err',Offline:'Off'};
  return `${short[word]}${context} ${selected}`.slice(0,13);
}
/** ASCII/13-byte device conversion remains the output profile's responsibility. */
export function rotoFeedback(pins:PinSettings, lanes:readonly RotoLane[]):RotoSlot[] {
  return Array.from({length:PIN_COUNT},(_,index) => {
    const pin = pins.slots[index];
    const lane = lanes.find(l => l.address === pin?.address);
    // Protocol steps is 7-bit. Preserve generated order and real (possibly sparse) keys.
    const sessions = lane?.sessions.slice(0,127) ?? [];
    return {index, name:(lane?.name ?? pin?.address.split('/').at(-1) ?? `#${index+1}`).slice(0,13),
      color:pin ? [1,3,5].map(n=>parseInt(pins.colors[index].slice(n,n+2),16)) : [0,0,0],
      id:pin?.address ?? `empty:${index}`, status: pin ? parameterText(lane?.activity ?? 'offline',lane?.contextPercent ?? null,
        sessions.find(s=>s.key===lane?.focused)?.name ?? '', sessions.length) : '',
      steps:sessions.length, value:sessionValue(sessions.findIndex(s=>s.key===lane?.focused),sessions.length),
      labels:sessions.map(s=>s.name.slice(0,13))};
  });
}
