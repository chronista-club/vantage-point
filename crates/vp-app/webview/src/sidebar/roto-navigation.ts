import {createSignal} from 'solid-js';
import {pinPadHandler} from '../../midi-pins';
import {rotoFeedback, sessionIndex} from '../../roto-pins';
import type {FleetPayload} from '../../fleet';
import {sessionNowKey} from '../../session-now-bridge';
import {readSessionContext} from '../../session-context-bridge';
import {createAgentActivity} from './ResponsePoint';
import {laneAddressKey, laneConnector} from './lane';
import {sidebar} from './store';
import {pins} from './midi-pin-store';
import {sendIpc} from './ipc';
import {pinLabel, selectLaneByNumber} from './actions/handlers';

/** Call within the sidebar root: activity and pins share its reactive ownership. */
export function createRotoNavigation() {
  const [contextRevision,setContextRevision] = createSignal(0);
  const [touched,setTouched] = createSignal<ReadonlySet<number>>(new Set());
  window.addEventListener('vp:session-context',()=>setContextRevision(n=>n+1));
  const laneAt = (index:number) => {
    const pin = pins().slots[index];
    return pin ? sidebar.lanes_by_repo[pin.path]?.find(l=>laneAddressKey(l)===pin.address) : undefined;
  };
  const activities = Array.from({length:8},(_,index)=>createAgentActivity({
    get lane(){return laneAt(index);},
    get session(){return laneAt(index)?.sessions?.focused;},
    get connectorClass(){const lane=laneAt(index);return lane ? laneConnector(lane,!!sidebar.awaiting_input[laneAddressKey(lane)]) : undefined;},
  }));
  let selectPin = pinPadHandler(selectLaneByNumber);
  window.addEventListener('vp:midi-target',()=>{
    selectPin = pinPadHandler(selectLaneByNumber);
    setTouched(new Set<number>());
  });
  function handle(payload:FleetPayload):boolean {
    const e = payload.event;
    if (!payload.port_name?.includes('Roto') || !e || !Number.isInteger(e.index) || e.index<0 || e.index>=8) return false;
    if(e.type==='button') return selectPin({port_name:'LPD8',event:{type:'pad',index:e.index,velocity:e.pressed ? 127 : 0}});
    if(e.type==='knob_touch') {
      const next = new Set(touched());
      if(e.pressed) next.add(e.index); else next.delete(e.index);
      setTouched(next); return true;
    }
    if(e.type!=='knob') return false;
    const pin=pins().slots[e.index], lane=laneAt(e.index);
    const sessions=lane?.sessions?.sessions.slice(0,127) ?? [];
    const index=sessionIndex(e.value ?? NaN,sessions.length);
    if(!pin || index===null) return true;
    const session=sessions[index].key;
    if(session===lane?.sessions?.focused) return true;
    document.dispatchEvent(new CustomEvent('vp:session-select',{detail:{lane:pin.address,session}}));
    sendIpc({t:'lane:select',path:pin.path,address:pin.address});
    window.ipc?.postMessage(JSON.stringify({t:'conversation:session_focus',lane:pin.address,session}));
    return true;
  }
  function feedback() {
    contextRevision();
    const lanes = pins().slots.flatMap((pin,index)=>{
      const lane=laneAt(index);
      if(!pin || !lane) return [];
      const focused=lane.sessions?.focused ?? lane.sessions?.root ?? 1;
      return [{address:pin.address,name:pinLabel(pin),focused,
        sessions:(lane.sessions?.sessions ?? []).map(s=>({key:s.key,name:sidebar.session_titles?.[sessionNowKey(pin.address,s.key)] || `cc#${s.key}`})),
        activity:activities[index].phase(),contextPercent:readSessionContext(pin.address,focused)}];
    });
    return rotoFeedback(pins(),lanes).map(slot=>({...slot,touched:touched().has(slot.index)}));
  }
  return {handle,feedback};
}
