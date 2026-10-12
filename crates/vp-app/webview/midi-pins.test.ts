import { expect, it } from 'vitest';
import * as pins from './midi-pins';
const a = {path:'/a',address:'a/root'};
const b = {path:'/b',address:'b/sub'};

it('only the MIDI target sends feedback and the pins own all eight pad LEDs', () => {
  const gallery = {knobs:[{index:0,value:0.5}],fader:null,pads:[{index:0,filled:true},{index:4,filled:false}]};
  const pads = [{index:0,filled:true,color:[1,2,3]}];
  expect(pins.navigationFeedback(gallery,{enabled:false,pads})).toBeNull();
  // Scene slot の点灯（gallery の pads）は残さない — pad 1〜8 は全部ピン
  expect(pins.navigationFeedback(gallery,{enabled:true,pads})).toEqual({...gallery,pads});
});

it('moves a pin without duplicates, replaces slots, and preserves custom colors and brightness', () => {
  let state = {...pins.defaultPins(), dimGain: 0.5};
  state.colors[0] = '#123456';
  state = pins.assignPin(state,0,a);
  state = pins.assignPin(state,7,a);
  expect(state.slots).toEqual([null,null,null,null,null,null,null,a]);
  state = pins.assignPin(state,7,b);
  expect(state.slots[7]).toEqual(b);
  expect(state.colors[0]).toBe('#123456');
  expect(state.dimGain).toBe(0.5);
  expect(pins.assignPin(state,7,null).slots).toEqual(Array(8).fill(null));
  expect(pins.assignPin(state,8,a)).toBe(state);
});

it('one source of numbers: pin #N ⇄ lane address (sidebar #N, ⌘N and pad N all read this)', () => {
  const state = pins.assignPin(pins.assignPin(pins.defaultPins(),0,a),7,b);
  expect(pins.pinNumberOf(state,'a/root')).toBe(1);
  expect(pins.pinNumberOf(state,'b/sub')).toBe(8); // sub lane も登録できる
  expect(pins.pinNumberOf(state,'c/root')).toBeNull(); // 未登録の行は番号を持たない
  expect(pins.pinTargetOf(state,1)).toEqual(a);
  expect(pins.pinTargetOf(state,8)).toEqual(b);
  expect(pins.pinTargetOf(state,2)).toBeNull();
  for (const n of [0,9,1.5,-1]) expect(pins.pinTargetOf(state,n)).toBeNull();
  // 登録を動かせば番号も動く（並び順は関係ない）
  expect(pins.pinNumberOf(pins.assignPin(state,3,a),'a/root')).toBe(4);
});

it('pads 1–8 ask for numbers 1–8 (= ⌘1–⌘8) once per press', () => {
  const selected: number[] = [];
  const handle = pins.pinPadHandler(n => selected.push(n));
  const pad = (index:number,velocity:number) => ({port_name:'LPD8 mk2',event:{type:'pad',index,velocity}});
  expect(handle(pad(0,127))).toBe(true);
  handle(pad(0,100));
  handle(pad(0,0));
  expect(selected).toEqual([1]);
  handle(pad(0,90));
  expect(handle(pad(7,127))).toBe(true);
  // 未登録でも pad は消費する（gallery へ流さない）。何もしないのは ⌘N と同じ関数の責務
  expect(handle(pad(3,127))).toBe(true);
  expect(selected).toEqual([1,1,8,4]);
  expect(handle(pad(8,127))).toBe(false);
  expect(handle({port_name:'X-Touch',event:{type:'pad',index:0,velocity:127}})).toBe(false);
});

it('restores only valid settings and projects selected, dimmed and empty pad colors', () => {
  const state = pins.assignPin(pins.defaultPins(),0,a);
  expect(pins.restorePins(JSON.parse(JSON.stringify(state)))).toEqual(state);
  expect(pins.restorePins({slots:[],colors:['bad']})).toEqual(pins.defaultPins());
  const feedback = pins.pinFeedback(state,'a/root',new Set(['a/root']));
  expect(feedback).toHaveLength(8);
  expect(feedback[0]).toMatchObject({index:0,filled:true,color:[68,194,181]});
  expect(feedback[1]).toMatchObject({index:1,filled:false,color:[0,0,0]});
  expect(feedback[7].index).toBe(7);
  expect(pins.pinFeedback(state,'other',new Set(['a/root']))[0].color).toEqual([17,49,45]);
  expect(pins.pinFeedback(state,'a/root',new Set())[0].color).toEqual([0,0,0]);
});

it('the unselected brightness comes from dimGain, not a constant', () => {
  const state = {...pins.assignPin(pins.defaultPins(),0,a), dimGain: 0.5};
  expect(pins.pinFeedback(state,'other',new Set(['a/root']))[0].color).toEqual([34,97,91]);
  expect(pins.pinFeedback({...state,dimGain:0},'other',new Set(['a/root']))[0].color).toEqual([0,0,0]);
  // 選択中は dimGain に関係なく設定色そのもの
  expect(pins.pinFeedback({...state,dimGain:0},'a/root',new Set(['a/root']))[0].color).toEqual([68,194,181]);
});

it('reads the old 4-pin session: keeps #1–#4, fills #5–#8 empty with default colors, dimGain 25%', () => {
  const old = {slots:[a,null,null,b],colors:['#112233','#445566','#778899','#aabbcc']};
  const restored = pins.restorePins(old);
  expect(restored.slots).toEqual([a,null,null,b,null,null,null,null]);
  expect(restored.colors).toEqual(['#112233','#445566','#778899','#aabbcc',...pins.DEFAULT_PIN_COLORS.slice(4)]);
  expect(restored.dimGain).toBe(0.25);
});

it('keeps pins when only dimGain is broken, and clamps nothing silently', () => {
  const saved = {...pins.assignPin(pins.defaultPins(),2,a)};
  for (const dimGain of [2,-0.1,Number.NaN,'0.5',null]) {
    const restored = pins.restorePins({...saved,dimGain});
    expect(restored.slots[2]).toEqual(a);
    expect(restored.dimGain).toBe(0.25);
  }
  expect(pins.restorePins({...saved,dimGain:0.6}).dimGain).toBe(0.6);
});

it('eight default colors keep the original four and are all distinct', () => {
  const c = pins.DEFAULT_PIN_COLORS;
  expect(c).toHaveLength(8);
  expect(c.slice(0,4)).toEqual(['#44c2b5','#639af5','#b78aee','#f0b45a']);
  expect(new Set(c.map(x => x.toLowerCase())).size).toBe(8);
});

it('color and brightness edits return a new state and ignore invalid input', () => {
  const state = pins.assignPin(pins.defaultPins(),5,a);
  const colored = pins.withPinColor(state,5,'#010203');
  expect(colored.colors[5]).toBe('#010203');
  expect(colored.slots[5]).toEqual(a);
  expect(state.colors[5]).toBe(pins.DEFAULT_PIN_COLORS[5]);
  expect(pins.withPinColor(state,8,'#010203')).toBe(state);
  expect(pins.withPinColor(state,1,'red')).toBe(state);
  expect(pins.withDimGain(state,0.4).dimGain).toBe(0.4);
  expect(pins.withDimGain(state,1.5)).toBe(state);
});
