// mem_1Cg5i626zphsJ9iXXXWCTE
import { afterEach, beforeAll, expect, it } from 'vitest';
import { build } from 'esbuild';
import { solidPlugin } from 'esbuild-plugin-solid';
import { Window } from 'happy-dom';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// Generated and checked by tests/midi_sidebar_wire.rs using SidebarState serialization.
const wire = JSON.parse(readFileSync(new URL('./fixtures/midi-sidebar-wire.json', import.meta.url), 'utf8'));
let bundle: string;
const windows: Window[] = [];
beforeAll(async () => {
  const result = await build({stdin:{contents:`
    import './src/sidebar/midi-navigation';
    import { applySidebarState } from './src/sidebar/store';
    window.applySidebarState = applySidebarState;
  `,resolveDir:fileURLToPath(new URL('.', import.meta.url))},bundle:true,write:false,
    preserveSymlinks:true,plugins:[solidPlugin()],format:'iife',platform:'browser',define:{'process.env.NODE_ENV':'"production"'}});
  bundle = result.outputFiles[0].text;
});

it.each(['component_active','no_active_lane','lane_not_found','settings_not_ready','target_inactive'])(
  'explains rejected registration: %s', reason => {
    const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
    windows.push(w);
    w.eval('window.sent=[];window.diagnostics=[];console.info=(...args)=>window.diagnostics.push(args);window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))}');
    if (reason !== 'target_inactive') w.eval('document.documentElement.dataset.midiTarget="true"');
    if (reason !== 'settings_not_ready') w.eval('window.__vpMidiSettings=null');
    w.eval(bundle);
    const state = reason === 'component_active' ? wire.devices : reason === 'no_active_lane' ? wire.startup :
      reason === 'lane_not_found' ? {...wire.lane,lanes_by_repo:{}} : wire.lane;
    w.eval(`window.applySidebarState(${JSON.stringify(state)})`);
    w.dispatchEvent(new w.CustomEvent('vp:midi-navigation',{
      cancelable:true,detail:{port_name:'LPD8 mk2',event:{type:'pad',index:3,velocity:127}},
    }));
    expect(w.eval('window.sent')).toEqual([]);
    const diagnostics = w.eval('window.diagnostics') as [string,{reason:string;pin:number;pinsReady:boolean;active_component:unknown;active_lane_address:unknown;repo_keys:string[];color?:string}][];
    const diagnostic = diagnostics.find(([tag, data]) => tag === '[midi:pin]' && data.reason === reason)?.[1];
    expect(diagnostic).toEqual(expect.objectContaining({
      reason,pin:4,pinsReady:reason !== 'settings_not_ready',
      active_component:state.active_component ?? null,
      active_lane_address:state.active_lane_address ?? null,
      repo_keys:Object.keys(state.lanes_by_repo),
    }));
    if (!['settings_not_ready','target_inactive'].includes(reason)) {
      expect(diagnostic!.color).toBe('#ff3030');
      expect(w.eval('window.__vpMidiPads.pads[3].color')).toEqual([255,48,48]);
    }
  },
);
afterEach(async () => { for (const w of windows.splice(0)) await w.happyDOM.abort(); });

it.each([false,true])('registers an empty pad after Rust sidebar pushes (Devices first: %s)', devicesFirst => {
  const w = new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}});
  windows.push(w);
  w.eval('window.sent=[];window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))};document.documentElement.dataset.midiTarget="true";window.__vpMidiSettings=null');
  w.eval(bundle);
  const push = (state:unknown) => w.eval(`window.applySidebarState(${JSON.stringify(state)})`);
  push(wire.startup);
  w.dispatchEvent(new w.CustomEvent('vp:midi-settings',{detail:null}));
  if (devicesFirst) push(wire.devices);
  push(wire.lane);
  for (const index of [3,7]) {
    for (const velocity of [127,0]) w.dispatchEvent(new w.CustomEvent('vp:midi-navigation',{
      cancelable:true,detail:{port_name:'LPD8 mk2',event:{type:'pad',index,velocity}},
    }));
    const sent = w.eval('window.sent') as {t:string;settings:string}[];
    const saved = sent.filter(m => m.t === 'midi:navigation').at(-1);
    expect(saved).toBeDefined();
    expect(JSON.parse(saved!.settings).slots[index]).toEqual({path:'/repos/vantage-point',address:'vantage-point/lane/roto-pins'});
    const pads = w.eval('window.__vpMidiPads') as {enabled:boolean;pads:{color:number[]}[]};
    expect(pads.enabled).toBe(true);
    expect(pads.pads[index].color).toEqual(index === 3 ? [240,180,90] : [228,122,201]);
  }
});
