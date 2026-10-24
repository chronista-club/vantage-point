// mem_1Cg7MpHWBdf6u9CYjyEAaf
import { afterEach, beforeAll, expect, it } from 'vitest';
import { build } from 'esbuild';
import { solidPlugin } from 'esbuild-plugin-solid';
import { Window } from 'happy-dom';
import { fileURLToPath } from 'node:url';

let bundle: string;
const windows: Window[] = [];
beforeAll(async () => {
  const result = await build({ stdin: { contents: `
    import { render } from 'solid-js/web';
    import { createComponent } from 'solid-js';
    import { Shell } from './src/sidebar/Shell';
    import { applySidebarState, emptyState } from './src/sidebar/store';
    import { clockNow } from './src/sidebar/activity-freshness';
    import { emitAgentActivity } from './session-activity-bridge';
    import wire from './fixtures/lane-progress-wire.json';
    const lane = (name, phase) => {
      const data = JSON.parse(JSON.stringify(wire[phase]));
      data.address = {repo:'project',name,key:'project/lane/'+name};
      data.sessions.sessions[0].last_activity_at = clockNow()-120000;
      return data;
    };
    window.setRequesting = requesting => applySidebarState({...emptyState(),
      processes:[{path:'/project',name:'Project',state:'running',expanded:true}],
      lanes_by_repo:{'/project':[lane('lead','waiting'),lane('feature',requesting?'requesting':'working')]},
      lane_inboxes:{'project/lane/feature':{unread_count:0,has_persistent:false}},
      active_lane_address:'project/lane/lead'});
    window.setDetached = () => applySidebarState({...emptyState(),
      processes:[{path:'/project',name:'Project',state:'running',expanded:true}],
      lanes_by_repo:{'/project':[{...lane('lead','waiting'),branch:null,
        sub_status:{branch:null,last_commit:'a1b2c3d fixture',dirty_count:2,behind:4,ahead:0}}]},
      active_lane_address:'project/lane/lead'});
    window.setRestored = () => {
      const restored = lane('feature','waiting');
      restored.flow_state = 'working';
      restored.pid = null;
      restored.sessions.sessions.forEach(session => {
        delete session.progress;
        delete session.last_response_at;
      });
      applySidebarState({...emptyState(),
        processes:[{path:'/project',name:'Project',state:'running',expanded:true}],
        lanes_by_repo:{'/project':[lane('lead','waiting'),restored]},
        active_lane_address:'project/lane/lead'});
    };
    window.emitLiveWork = () => emitAgentActivity('project/lane/feature', wire.waiting.sessions.root,
      {kind:'message_chunk',text:'実際の出力'});
    window.setRequesting(true);
    render(() => createComponent(Shell,{}),document.getElementById('sidebar-root'));
  `, loader:'tsx', resolveDir:fileURLToPath(new URL('.',import.meta.url)) },
    bundle:true,write:false,format:'iife',platform:'browser',preserveSymlinks:true,plugins:[solidPlugin()],
    define:{'process.env.NODE_ENV':'"production"'} });
  bundle = result.outputFiles[0].text;
});
function fixture() {
  const win = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(win);
  win.document.body.innerHTML='<div id="sidebar-root"></div><div id="host"></div>';
  win.eval(bundle);
  return win;
}
afterEach(async()=>{for(const win of windows.splice(0)) await win.happyDOM.abort();});

it('keeps a restored GUI lane waiting when only its wire connector implies work', () => {
  const win = fixture();
  (win as unknown as {setRestored: () => void}).setRestored();
  const row = win.document.querySelectorAll('.vp-lane-row')[1];
  expect(row.querySelector('[data-lane-state="waiting"]')).not.toBeNull();
  expect(row.querySelector('[data-lane-state="working"]')).toBeNull();
  (win as unknown as {emitLiveWork: () => void}).emitLiveWork();
  expect(row.querySelector('[data-lane-state="working"]')).not.toBeNull();
});

it('shows request, now-line and elapsed time for a lane whose chat is not mounted',()=>{
  const win=fixture();
  const row=win.document.querySelectorAll('.vp-lane-row')[1];
  expect(row.querySelector('[data-lane-state="requesting"]')).not.toBeNull();
  expect(row.textContent).toContain('Bash の承認をお願いします');
  expect(row.querySelector('.vp-lane-elapsed')?.textContent).toBe('2m');
  expect(row.querySelector('.vp-lane-cwd')).toBeNull();
  expect(win.document.querySelector('.vp-sidebar-requests')?.textContent).toContain('1');
});

it('clears request counts on the next snapshot after a response',()=>{
  const win=fixture();
  (win as unknown as {setRequesting(value:boolean):void}).setRequesting(false);
  expect(win.document.querySelector('[data-lane-state="requesting"]')).toBeNull();
  expect(win.document.querySelector('.vp-sidebar-requests')).toBeNull();
  const row=win.document.querySelectorAll('.vp-lane-row')[1];
  expect(row.querySelector('[data-lane-state="working"]')).not.toBeNull();
  expect(row.textContent).toContain('回帰テストを確認中');
});

it('keeps detached commit and dirty information on the branch line',()=>{
  const win=fixture();
  (win as unknown as {setDetached():void}).setDetached();
  const row=win.document.querySelector('.vp-lane-row')!;
  expect(row.querySelector('.vp-lane-git')?.textContent).toContain('detached @ a1b2c3d');
  expect(row.querySelector('.vp-lane-git')?.textContent).toContain('dirty');
  expect(row.querySelector('.vp-lane-git')?.textContent).toContain('↓4');
});

it('shows a PR badge only for the branch reported by the right sidebar',()=>{
  const win=fixture();
  const emit = (branch:string) => win.document.dispatchEvent(new win.CustomEvent('vp:lane-prs-count',{
    detail:{lane:'project/lane/feature',count:1,available:true,branch,number:123},
  }));
  emit('wip/requesting');
  expect(win.document.querySelector('.vp-lane-pr')?.textContent).toBe('#123');
  emit('wip/old');
  expect(win.document.querySelector('.vp-lane-pr')).toBeNull();
});


it('opens wire history even when all messages have been read',()=>{
  const win=fixture();
  const opened: string[]=[];
  (win as unknown as {vpWire:unknown}).vpWire={open:(lane:string)=>opened.push(lane)};
  const row=win.document.querySelectorAll('.vp-lane-row')[1];
  const mailbox=row.querySelector('button[title="wire inbox を開く"]');
  expect(mailbox).not.toBeNull();
  (mailbox as unknown as {click():void}).click();
  expect(opened).toEqual(['project/lane/feature']);
});
