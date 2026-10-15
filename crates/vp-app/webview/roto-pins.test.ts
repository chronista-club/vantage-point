// mem_1Cg4LYjWJGpLpLx4LQnjDz
import {describe, it, expect} from 'vitest';
import {defaultPins, assignPin} from './midi-pins';
import {rotoFeedback, sessionIndex, sessionValue, statusText, parameterText} from './roto-pins';

describe('ROTO pin projection', () => {
  it('preserves all eight empty slots with dark color and no invented session', () => {
    const slots = rotoFeedback(defaultPins(), []);
    expect(slots).toHaveLength(8);
    expect(slots[7]).toMatchObject({index:7, name:'#8', color:[0,0,0], steps:0, value:0, labels:[]});
  });
  it('uses pin order, color, focused session and custom title, not lane order', () => {
    const pins = assignPin(defaultPins(), 3, {path:'/repo',address:'vp/topic'});
    const slots = rotoFeedback(pins, [{address:'vp/topic',name:'abcdefghijklmnop',focused:9,
      sessions:[{key:2,name:'cc#2'},{key:9,name:'review'}],activity:'waiting',contextPercent:42}]);
    expect(slots[3]).toMatchObject({index:3,name:'abcdefghijklmnop',color:[240,180,90],steps:2,value:1,
      labels:['cc#2','review'],status:'Ask 42%'});
    expect(slots[0].name).toBe('#1');
  });
  it('retains missing lane identity but does not invent its state', () => {
    const pins = assignPin(defaultPins(),0,{path:'/repo',address:'vp/gone'});
    expect(rotoFeedback(pins,[])[0]).toMatchObject({name:'gone',status:'Offline --%',steps:0});
  });
});
describe('session detents', () => {
  it('round trips every detent, including one session and sparse keys', () => {
    for(const count of [1,2,3,8,16,127]) for(let index=0;index<count;index++)
      expect(sessionIndex(sessionValue(index,count),count)).toBe(index);
    expect(sessionIndex(0.5,0)).toBeNull();
    expect(sessionIndex(NaN,3)).toBeNull();
    expect(sessionIndex(-1,3)).toBe(0);
    expect(sessionIndex(2,3)).toBe(2);
  });
  it('distinguishes human attention from idle, and unknown context from zero', () => {
    expect(statusText('thinking',0)).toBe('Think 0%');
    expect(statusText('working',72)).toBe('Work 72%');
    expect(statusText('completed',null)).toBe('Idle --%');
    expect(statusText('waiting',100)).toBe('Ask 100%');
  });
});


it('fits state, context and current session into the learn name above 16 sessions', () => {
  expect(parameterText('waiting',42,'review',17)).toBe('Ask42% review');
  expect(parameterText('thinking',100,'cc#127',127)).toBe('Th100% cc#127');
  expect(parameterText('working',null,'very-long-session-name',17)).toBe('Wk--% very-lo');
  expect(parameterText('waiting',42,'review',16)).toBe('Ask 42%');
});

it('passes full Unicode names to the profile so ASCII conversion happens before byte truncation', () => {
  const pins = assignPin(defaultPins(),0,{path:'/repo',address:'vp/topic'});
  const name = '日本語日本語日本語abcdefghijklmnop';
  expect(rotoFeedback(pins,[{address:'vp/topic',name,focused:1,sessions:[{key:1,name}],activity:'idle',contextPercent:null}])[0])
    .toMatchObject({name,labels:[name]});
});
