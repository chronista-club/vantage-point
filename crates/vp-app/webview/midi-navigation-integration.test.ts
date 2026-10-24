import { afterEach, beforeAll, expect, it, vi } from 'vitest';
import { build } from 'esbuild';
import { solidPlugin } from 'esbuild-plugin-solid';
import { Window } from 'happy-dom';
import { fileURLToPath } from 'node:url';

let bundle: string;
const windows: Window[] = [];
beforeAll(async () => {
  const result = await build({ stdin: { contents: `
    import './src/sidebar/midi-navigation';
    import { setPinColor } from './src/sidebar/midi-pin-store';
    window.setPinColor = setPinColor;
    import { render } from 'solid-js/web';
    import { MidiPins } from './src/sidebar/MidiPins';
    window.renderPins = () => { const el = document.createElement('div'); document.body.append(el); render(MidiPins, el); return el; };
    import { applySidebarState, emptyState, sidebar } from './src/sidebar/store';
    import { emitAgentActivity } from './session-activity-bridge';
    window.emitAgentActivity = emitAgentActivity;
    window.applyTestState = value => applySidebarState({...emptyState(),...value});
    import { selectLaneByNumber } from './src/sidebar/actions/handlers';
    window.selectLaneByNumber = selectLaneByNumber;
    window.clearActiveLane = () => applySidebarState({...sidebar, active_lane_address:null});
    window.sent = [];
    window.ipc = {postMessage: m => window.sent.push(JSON.parse(m))};
    const lane = name => ({address:{repo:'vp',name,key:'vp/'+name}});
    applySidebarState({...emptyState(), processes:[{path:'/vp',name:'vp',state:'running',expanded:false}],
      lanes_by_repo:{'/vp':[lane('root'),lane('sub')]}, active_lane_address:'vp/root'});
    window.dispatchEvent(new CustomEvent('vp:midi-target',{detail:true}));
  `, resolveDir: fileURLToPath(new URL('.', import.meta.url)) }, bundle:true, write:false,
    preserveSymlinks:true,plugins:[solidPlugin()], format:'iife', platform:'browser', define:{'process.env.NODE_ENV':'"production"'} });
  bundle = result.outputFiles[0].text;
});
afterEach(async () => { vi.useRealTimers(); for (const w of windows.splice(0)) await w.happyDOM.abort(); });

it('selects collapsed lanes through real sidebar IPC and consumes the gesture before gallery', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  const send = (index:number,value:number) => w.dispatchEvent(new w.CustomEvent('vp:midi-navigation', {
    cancelable:true, detail:{port_name:'LPD8 mk2',event:{type:'knob',index,value}},
  }));
  expect(send(0,1)).toBe(false);
  expect(w.sent).toEqual([{t:'process:toggle',path:'/vp',expanded:true}, {t:'lane:select',path:'/vp',address:'vp/sub'}]);
  expect(send(0,0.99)).toBe(false);
  expect(w.sent).toHaveLength(2);
  expect(send(1,1)).toBe(true);
});

it('restores shared pins, selects on release, and registers empty pads through IPC', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  // 旧 4 ピンの session — #1〜#4 は pad 1〜4 に載る
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:{
    slots:[{path:'/vp',address:'vp/sub'},null,null,{path:'/gone',address:'gone/root'}],
    colors:['#112233','#445566','#778899','#aabbcc'],
  }}));
  const send = (index:number,velocity:number) => w.dispatchEvent(new w.CustomEvent('vp:midi-navigation', {
    cancelable:true, detail:{port_name:'LPD8 mk2',event:{type:'pad',index,velocity}},
  }));
  expect(send(0,127)).toBe(false);
  expect(w.sent).toEqual([]);
  send(0,100);
  send(0,0);
  expect(w.sent).toEqual([{t:'process:toggle',path:'/vp',expanded:true},{t:'lane:select',path:'/vp',address:'vp/sub'}]);
  send(1,127);
  expect(w.sent).toHaveLength(3);
  expect(JSON.parse((w.sent[2] as {settings:string}).settings).slots[1]).toEqual({path:'/vp',address:'vp/root'});
  send(3,127);
  send(3,0);
  expect(w.sent).toHaveLength(3);
  send(7,127);
  expect(w.sent).toHaveLength(4);
  // pad 5〜8 も gallery には流さない（ピン #5〜#8）
  expect(send(4,127)).toBe(false);
});

it('registered pad N and ⌘N go to the same lane; missing lanes do nothing', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  const slots = [{path:'/vp',address:'vp/root'},{path:'/vp',address:'vp/sub'},null,{path:'/gone',address:'gone/root'},null,null,null,null];
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:{slots,colors:Array(8).fill('#123456'),dimGain:0.25}}));
  const pad = (index:number,velocity:number) => w.dispatchEvent(new w.CustomEvent('vp:midi-navigation', {
    cancelable:true, detail:{port_name:'LPD8 mk2',event:{type:'pad',index,velocity}},
  }));
  const viaPad = (n:number) => { w.sent.length = 0; pad(n - 1, 127); pad(n - 1, 0); return [...w.sent]; };
  const viaCmd = (n:number) => { w.sent.length = 0; w.eval(`window.selectLaneByNumber(${n})`); return [...w.sent]; };
  for (const n of [1,2,4]) expect(viaPad(n)).toEqual(viaCmd(n));
  expect(viaCmd(2)).toEqual([{t:'process:toggle',path:'/vp',expanded:true},{t:'lane:select',path:'/vp',address:'vp/sub'}]);
  expect(viaCmd(3)).toEqual([]); // 未登録
  expect(viaCmd(4)).toEqual([]); // 登録した lane が今いない — 別の lane に振り替えない
});

it('the strip registers the selected lane on an empty pin and unregisters with the hover ✕', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: {t:string;settings?:string}[]};
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:null}));
  const root = w.eval('window.renderPins()') as HTMLElement;
  const pinButton = (n:number) => Array.from(root.querySelectorAll('.vp-midi-pin')).find(el => el.querySelector('button > span')?.textContent === `#${n}`)!;
  expect(pinButton(3).querySelector('.vp-midi-unpin')).toBeNull();
  (pinButton(3).querySelector('button') as HTMLButtonElement).click(); // active = vp/root
  expect(JSON.parse(w.sent.at(-1)!.settings!).slots[2]).toEqual({path:'/vp',address:'vp/root'});
  expect(pinButton(3).querySelector('.vp-midi-pin-label')!.textContent).toBe('vp');
  (pinButton(3).querySelector('.vp-midi-unpin') as HTMLButtonElement).click();
  expect(JSON.parse(w.sent.at(-1)!.settings!).slots[2]).toBeNull();
  expect(pinButton(3).querySelector('.vp-midi-unpin')).toBeNull();
});

it('the sidebar pin strip and LED feedback follow settings re-broadcast from native', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true"');
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  const slots = [{path:'/vp',address:'vp/root'},{path:'/vp',address:'vp/sub'},null,null,null,null,null,null];
  const colors = ['#ff0000','#00ff00','#0000ff','#ffffff','#ffffff','#ffffff','#ffffff','#ffffff'];
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:{slots,colors,dimGain:0.5}}));
  const pads = () => w.eval('window.__vpMidiPads') as {enabled:boolean;pads:{index:number;color:number[]}[]};
  expect(pads().enabled).toBe(true);
  expect(pads().pads[0].color).toEqual([255,0,0]); // active = vp/root
  expect(pads().pads[1].color).toEqual([0,128,0]); // 未選択 = dimGain 0.5
  // Editor が変えた値が Rust 経由で戻ってくる — 同じ経路で LED が追随する
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:{slots,colors,dimGain:0.1}}));
  expect(pads().pads[1].color).toEqual([0,26,0]);
});

it('changing a color from the sidebar keeps the brightness chosen in the Editor', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: {t:string;settings?:string}[]};
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  const settings = {slots:Array(8).fill(null),colors:Array(8).fill('#123456'),dimGain:0.6};
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:settings}));
  w.eval('window.setPinColor(7,"#abcdef")');
  const saved = JSON.parse(w.sent.at(-1)!.settings!);
  expect(saved).toEqual({...settings,colors:[...settings.colors.slice(0,7),'#abcdef']});
});

it('initializes new windows when native settings arrive before the sidebar bundle', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval('window.__vpMidiSettings=null;document.documentElement.dataset.midiTarget="true"');
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  expect(w.eval('window.__vpMidiPads.enabled')).toBe(true);
});

it('the pin strip shows 8 pins in two rows like the pads: #5–#8 above #1–#4', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:null}));
  const labels = w.eval(`Array.from(window.renderPins().querySelectorAll('.vp-midi-pin button > span:first-child')).map(s => s.textContent).join(',')`);
  expect(labels).toBe('#5,#6,#7,#8,#1,#2,#3,#4');
});


it('ROTO buttons 1–8 select pins once per press and never start voice', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);w.eval(bundle);
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings',{detail:{slots:[{path:'/vp',address:'vp/sub'},null,null,null,null,null,null,{path:'/vp',address:'vp/root'}],colors:Array(8).fill('#123456')}}));
  const ptt: boolean[]=[];
  w.addEventListener('vp:roto-ptt',e=>ptt.push((e as unknown as CustomEvent).detail.pressed));
  const send=(type:string,index:number,pressed:boolean)=>w.dispatchEvent(new w.CustomEvent('vp:midi-navigation',{cancelable:true,detail:{port_name:'Roto-Control',event:{type,index,pressed,value:1}}}));
  send('button',0,true);send('button',0,true);send('button',0,false);
  expect(w.sent).toEqual([{t:'process:toggle',path:'/vp',expanded:true},{t:'lane:select',path:'/vp',address:'vp/sub'}]);
  w.sent.length=0;
  send('button',1,true);send('knob',0,true);send('knob_touch',0,true);
  send('button',7,true);send('button',7,true);send('button',7,false);send('button',7,false);
  expect(w.sent).toEqual([{t:'process:toggle',path:'/vp',expanded:true},{t:'lane:select',path:'/vp',address:'vp/root'}]);
  expect(ptt).toEqual([]);
  w.sent.length=0;
  expect(w.eval('window.__vpMidiPads.roto')).toBeUndefined();
  w.eval("window.dispatchEvent(new CustomEvent('vp:midi-target',{detail:false}))");
  send('button',0,true);send('button',7,true);
  expect(w.sent).toEqual([]);expect(ptt).toEqual([]);
});

it('holds through real IPC, shows rejection without a lane, and cancels on target loss', () => {
  vi.useFakeTimers();
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: {t:string;settings?:string}[]};
  windows.push(w);
  // Timers used inside the bundled browser module are driven by the same fake clock.
  w.setTimeout = setTimeout as unknown as typeof w.setTimeout;
  w.clearTimeout = clearTimeout as unknown as typeof w.clearTimeout;
  w.eval('document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  const settings = {slots:[{path:'/vp',address:'vp/sub'},...Array(7).fill(null)],colors:Array(8).fill('#123456'),dimGain:0.25};
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:settings}));
  const send = (index:number,velocity:number) => w.dispatchEvent(new w.CustomEvent('vp:midi-navigation', {
    cancelable:true, detail:{port_name:'LPD8 mk2',event:{type:'pad',index,velocity}},
  }));
  send(0,127);
  vi.advanceTimersByTime(599);
  expect(w.sent).toEqual([]);
  vi.advanceTimersByTime(1);
  expect(w.sent[0].t).toBe('midi:navigation');
  expect(JSON.parse(w.sent[0].settings!).slots[0]).toEqual({path:'/vp',address:'vp/root'});
  expect(w.eval('window.__vpMidiPads.pads[0].color')).toEqual([18,52,86]);
  vi.advanceTimersByTime(150);
  expect(w.eval('window.__vpMidiPads.pads[0].color')).toEqual([0,0,0]);
  send(0,0);
  expect(w.sent).toHaveLength(1);
  vi.advanceTimersByTime(450);
  expect(w.eval('window.__vpMidiPads.pads[0].color')).toEqual([18,52,86]);

  w.eval('window.clearActiveLane()');
  send(7,127);
  expect(w.sent).toHaveLength(1);
  expect(w.eval('window.__vpMidiPads.pads[7].color')).toEqual([255,48,48]);
  send(7,0);
  vi.advanceTimersByTime(600);
  expect(w.eval('window.__vpMidiPads.pads[7].color')).toEqual([0,0,0]);

  send(0,127);
  w.eval("window.dispatchEvent(new CustomEvent('vp:midi-target', {detail:false}))");
  vi.advanceTimersByTime(600);
  expect(w.eval('window.__vpMidiPads.enabled')).toBe(false);
  w.eval("window.dispatchEvent(new CustomEvent('vp:midi-target', {detail:true}))");
  send(0,0);
  expect(w.sent).toHaveLength(1);
});

it('consumes pads while settings load without blocking unrelated gallery controls', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true"');
  w.eval(bundle);
  const send = (type:string,index:number) => w.dispatchEvent(new w.CustomEvent('vp:midi-navigation', {
    cancelable:true, detail:{port_name:'LPD8 mk2',event:{type,index,velocity:127,value:1}},
  }));
  expect(send('pad',7)).toBe(false);
  expect(w.sent).toEqual([]);
  expect(send('knob',1)).toBe(true);
});

it('makes registration visible even when the configured pin color is black', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true"');
  w.eval(bundle);
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:{slots:Array(8).fill(null),colors:Array(8).fill('#000000'),dimGain:0}}));
  w.dispatchEvent(new w.CustomEvent('vp:midi-navigation', {cancelable:true,detail:{port_name:'LPD8 mk2',event:{type:'pad',index:7,velocity:127}}}));
  expect(w.eval('window.__vpMidiPads.pads[7].color')).toEqual([255,255,255]);
});
