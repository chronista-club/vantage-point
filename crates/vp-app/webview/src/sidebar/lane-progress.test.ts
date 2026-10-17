// mem_1Cg7MpHWBdf6u9CYjyEAaf
import { describe, expect, it } from 'vitest';
import type { LaneInfo } from '../generated/LaneInfo';
import { activityElapsed, laneDisplayState, requestingLaneCount } from './lane-progress';

function lane(phase: string, mode='gui', pid: number|null=42): LaneInfo {
  return {address:{repo:'vp',name:'feature',key:'vp/lane/feature'},state:'running',agent:'claude',pid,
    sessions:{root:3,focused:1,sessions:[{key:1,agent:'claude',mode,progress:{phase:'stopped',now:null,error:false}},
      {key:3,agent:'claude',mode,progress:{phase,now:null,error:false}}]}} as LaneInfo;
}
describe('laneDisplayState',()=>{
  it.each([
    ['requesting',0,'requesting'], ['working',0,'working'], ['waiting',0,'waiting'],
    ['waiting',2,'unread'], ['stopped',0,'stopped'], ['requesting',2,'requesting'],
    ['working',2,'working'], ['stopped',2,'stopped'],
  ])('%s + unread %s → %s', (phase,unread,expected)=>{
    expect(laneDisplayState(lane(String(phase)),Number(unread))).toBe(expected);
  });
  it('does not infer a TUI user request from chat or input-wait signals',()=>{
    expect(laneDisplayState(lane('requesting','tui'),0,'waiting')).toBe('waiting');
    expect(laneDisplayState(lane('waiting','tui',null))).toBe('stopped');
  });
  it('counts root sessions of chat lanes only',()=>{
    expect(requestingLaneCount([lane('requesting'),lane('requesting','tui'),lane('working')])).toBe(1);
  });
  it('keeps TUI working when vp now has supplied a text-only projection',()=>{
    expect(laneDisplayState({...lane('waiting','tui'),flow_state:'working'})).toBe('working');
  });
});
describe('activityElapsed',()=>{
  it.each([[null,'—'],[0,'—'],[NaN,'—'],[121000,'now'],[120000,'now'],[60001,'now'],[60000,'1m'],[1,'1m']])(
    '%s → %s',(at,expected)=>expect(activityElapsed(at,120000)).toBe(expected));
  it('uses minutes then hours at their boundaries',()=>{
    expect(activityElapsed(1,59*60000+1)).toBe('59m');
    expect(activityElapsed(1,60*60000+1)).toBe('1h');
  });
});

it('prefers a fresh daemon projection over a stale mounted-chat error',()=>{
  expect(laneDisplayState(lane('working'),0,'error')).toBe('working');
});
