import { describe, expect, it } from 'vitest';
import * as navigation from './midi-navigation';
import type { LaneInfo } from './src/generated/LaneInfo';

// mem_1Cfq8x3H5HHVsLDmvhYSKf
const lane = (repo: string, name: string) => ({
  address: { repo, name, key: `${repo}/${name}` },
}) as LaneInfo;

describe('LPD8 lane navigation', () => {
  it('follows CURRENTs order, root before children, regardless of collapse', () => {
    expect(navigation.currentLanes({
      processes: [
        {path:'/a',name:'a',expanded:false,state:'running'},
        {path:'/b',name:'b',expanded:true,state:'running'},
        {path:'/c',name:'c',expanded:true,state:'stopped'},
      ],
      currents_order: ['/b','/c','/a'],
      lanes_by_repo: {'/a':[lane('a','child'),lane('a','root')], '/b':[lane('b','root')], '/c':[lane('c','root')]},
    })).toEqual([
      {path:'/b',address:'b/root'}, {path:'/a',address:'a/root'}, {path:'/a',address:'a/child'},
    ]);
  });
  it('reaches every lane for 1 through 128 lanes, including both endpoints', () => {
    for (let count=1; count<=128; count++) {
      const indices = Array.from({length:128}, (_,v) => navigation.laneIndex(v/127,count));
      expect(indices[0]).toBe(0);
      expect(indices[127]).toBe(count-1);
      expect(new Set(indices).size).toBe(count);
    }
    expect(navigation.laneIndex(0,0)).toBeNull();
    expect(navigation.laneIndex(NaN,10)).toBeNull();
  });
  it('consumes only knob 1 and avoids repeated selection within the same interval', () => {
    const selected: string[] = [];
    const targets = [{path:'/a',address:'a/root'},{path:'/a',address:'a/sub'}];
    const handle = navigation.laneKnobHandler(() => targets, a => selected.push(a.address));
    const knob = (value:number,index=0,port_name='LPD8 mk2') => ({port_name,event:{type:'knob',index,value}});
    expect(handle(knob(0))).toBe(true);
    handle(knob(0.1));
    handle(knob(1));
    handle(knob(0));
    expect(selected).toEqual(['a/root','a/sub','a/root']);
    expect(handle(knob(1,1))).toBe(false);
    expect(handle(knob(1,0,'Roto-Control'))).toBe(false);
  });
});
