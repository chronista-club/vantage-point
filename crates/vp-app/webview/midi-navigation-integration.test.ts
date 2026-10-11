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

it('restores per-window pins and upper pads select the assigned lane through IPC', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent: unknown[]};
  windows.push(w);
  w.eval(bundle);
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings', {detail:{
    slots:[{path:'/vp',address:'vp/sub'},null,null,{path:'/gone',address:'gone/root'}],
    colors:['#112233','#445566','#778899','#aabbcc'],
  }}));
  const send = (index:number,velocity:number) => w.dispatchEvent(new w.CustomEvent('vp:midi-navigation', {
    cancelable:true, detail:{port_name:'LPD8 mk2',event:{type:'pad',index,velocity}},
  }));
  expect(send(4,127)).toBe(false);
  expect(w.sent).toEqual([{t:'process:toggle',path:'/vp',expanded:true},{t:'lane:select',path:'/vp',address:'vp/sub'}]);
  send(4,100);
  send(5,127);
  send(7,127);
  expect(w.sent).toHaveLength(2);
  expect(send(0,127)).toBe(true);
});

it('initializes new windows when native settings arrive before the sidebar bundle', () => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval('window.__vpMidiSettings=null;document.documentElement.dataset.midiTarget="true"');
  w.eval(bundle);
  expect(w.eval('window.__vpMidiPads.enabled')).toBe(true);
});
