import { afterEach, beforeAll, expect, it } from 'vitest';
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
    import { applySidebarState, emptyState } from './src/sidebar/store';
    window.sent = [];
    window.ipc = {postMessage: m => window.sent.push(JSON.parse(m))};
    const lane = name => ({address:{repo:'vp',name,key:'vp/'+name}});
    applySidebarState({...emptyState(), processes:[{path:'/vp',name:'vp',state:'running',expanded:false}],
      lanes_by_repo:{'/vp':[lane('root'),lane('sub')]}, active_lane_address:'vp/root'});
  `, resolveDir: fileURLToPath(new URL('.', import.meta.url)) }, bundle:true, write:false,
    plugins:[solidPlugin()], format:'iife', platform:'browser', define:{'process.env.NODE_ENV':'"production"'} });
  bundle = result.outputFiles[0].text;
});
afterEach(async () => { for (const w of windows.splice(0)) await w.happyDOM.abort(); });

it('selects collapsed lanes through real sidebar IPC and consumes the gesture before gallery', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);
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

it('restores per-window pins and pads 1–8 select the assigned lane through IPC', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);
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
  expect(w.sent).toEqual([{t:'process:toggle',path:'/vp',expanded:true},{t:'lane:select',path:'/vp',address:'vp/sub'}]);
  send(0,100);
  send(1,127);
  send(3,127);
  send(7,127);
  expect(w.sent).toHaveLength(2);
  // pad 5〜8 も gallery には流さない（ピン #5〜#8）
  expect(send(4,127)).toBe(false);
});

it('the sidebar pin strip and LED feedback follow settings re-broadcast from native', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval('document.documentElement.dataset.midiTarget="true"');
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
  w.eval(bundle);
  expect(w.eval('window.__vpMidiPads.enabled')).toBe(true);
});

it('the pin strip shows 8 pins in two rows like the pads: #5–#8 above #1–#4', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval(bundle);
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:null}));
  const labels = w.eval(`Array.from(window.renderPins().querySelectorAll('.vp-midi-pin button > span:first-child')).map(s => s.textContent).join(',')`);
  expect(labels).toBe('#5,#6,#7,#8,#1,#2,#3,#4');
});
