import { expect, it } from 'vitest';
import * as pins from './midi-pins';
const a = {path:'/a',address:'a/root'};
const b = {path:'/b',address:'b/sub'};

it('only the MIDI target sends feedback and upper pads replace gallery scene LEDs', () => {
  const gallery = {knobs:[{index:0,value:0.5}],fader:null,pads:[{index:0,filled:true},{index:4,filled:false}]};
  const pads = [{index:4,filled:true,color:[1,2,3]}];
  expect(pins.navigationFeedback(gallery,{enabled:false,pads})).toBeNull();
  expect(pins.navigationFeedback(gallery,{enabled:true,pads})).toEqual({...gallery,pads:[{index:0,filled:true},...pads]});
});

it('moves a pin without duplicates, replaces slots, and preserves custom colors', () => {
  let state = pins.defaultPins();
  state.colors[0] = '#123456';
  state = pins.assignPin(state,0,a);
  state = pins.assignPin(state,3,a);
  expect(state.slots).toEqual([null,null,null,a]);
  state = pins.assignPin(state,3,b);
  expect(state.slots[3]).toEqual(b);
  expect(state.colors[0]).toBe('#123456');
  expect(pins.assignPin(state,3,null).slots).toEqual([null,null,null,null]);
});

it('upper pads 5–8 select pins 1–4 once per press; lower pads retain their mapping', () => {
  const state = pins.assignPin(pins.defaultPins(),0,a);
  const selected: string[] = [];
  const handle = pins.pinPadHandler(() => state, p => selected.push(p.address));
  const pad = (index:number,velocity:number) => ({port_name:'LPD8 mk2',event:{type:'pad',index,velocity}});
  expect(handle(pad(0,127))).toBe(false);
  expect(handle(pad(4,127))).toBe(true);
  handle(pad(4,100));
  handle(pad(4,0));
  expect(selected).toEqual(['a/root']);
  handle(pad(4,90));
  handle(pad(7,127)); // unassigned remains inert and consumed
  expect(selected).toEqual(['a/root','a/root']);
});

it('restores only valid settings and projects selected, assigned and empty pad colors', () => {
  const state = pins.assignPin(pins.defaultPins(),0,a);
  expect(pins.restorePins(JSON.parse(JSON.stringify(state)))).toEqual(state);
  expect(pins.restorePins({slots:[],colors:['bad']})).toEqual(pins.defaultPins());
  const feedback = pins.pinFeedback(state,'a/root',new Set(['a/root']));
  expect(feedback[0]).toMatchObject({index:4,filled:true,color:[68,194,181]});
  expect(feedback[1]).toMatchObject({index:5,filled:false,color:[0,0,0]});
  expect(pins.pinFeedback(state,'other',new Set(['a/root']))[0].color).toEqual([17,49,45]);
  expect(pins.pinFeedback(state,'a/root',new Set())[0].color).toEqual([0,0,0]);
});
