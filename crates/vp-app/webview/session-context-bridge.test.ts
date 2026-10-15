// @vitest-environment happy-dom
import {it,expect} from 'vitest';
import {emitSessionContext, readSessionContext} from './session-context-bridge';
it('keeps per-session usage, preserves omitted fields, resets on initialization',()=>{
  emitSessionContext('vp/root',2,{kind:'turn_completed',context_tokens:40,context_window:100});
  expect(readSessionContext('vp/root',2)).toBe(40);
  expect(readSessionContext('vp/root',1)).toBeNull();
  emitSessionContext('vp/root',2,{kind:'turn_completed'});
  expect(readSessionContext('vp/root',2)).toBe(40);
  emitSessionContext('vp/root',2,{kind:'turn_completed',context_tokens:0});
  expect(readSessionContext('vp/root',2)).toBe(0);
  emitSessionContext('vp/root',2,{kind:'session_init'});
  expect(readSessionContext('vp/root',2)).toBeNull();
});
