import {sessionNowKey} from './session-now-bridge';
type Context = {tokens?:number; window?:number};
type ContextWindow = Window & {__vpSessionContext?:Record<string,Context>};
export function readSessionContext(lane:string, session:number):number|null {
  const c = (window as ContextWindow).__vpSessionContext?.[sessionNowKey(lane,session)];
  return c?.tokens != null && Number.isFinite(c.tokens) && c.window && Number.isFinite(c.window) && c.window > 0
    ? Math.round(Math.max(0,Math.min(100,c.tokens/c.window*100))) : null;
}
export function emitSessionContext(lane:string, session:number, event:{kind:string; context_tokens?:number; context_window?:number}):void {
  if (event.kind !== 'turn_completed' && event.kind !== 'session_init') return;
  const cache = ((window as ContextWindow).__vpSessionContext ??= {});
  const key = sessionNowKey(lane,session);
  const old = cache[key];
  cache[key] = event.kind === 'session_init' ? {} : {
    tokens:event.context_tokens ?? old?.tokens, window:event.context_window ?? old?.window};
  window.dispatchEvent(new CustomEvent('vp:session-context',{detail:{lane,session}}));
}
