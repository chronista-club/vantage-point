// mem_1Cfpw2kcDSU55mVQHzexKB — geometry, reversible editing and safe SVG export.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync, existsSync} from 'node:fs';
const source = readFileSync(new URL('../../assets/brand/source.svg', import.meta.url), 'utf8');
const modelUrl = new URL('./model.mjs', import.meta.url);
test('parametric model exists', () => assert.ok(existsSync(modelUrl), 'editable brand model must exist'));
test('current preset preserves canonical geometry; thinner preset reveals color', async () => {
  const {render, readParameters, COLOR_FIRST} = await import(modelUrl);
  const original = render(source, readParameters(source));
  assert.equal(original, source);
  const thin = render(source, COLOR_FIRST);
  assert.match(thin, /stroke-width="12" stroke-linejoin="round"/);
  assert.match(thin, /r="18" fill="#d4a73e" stroke="currentColor" stroke-width="8"/);
  assert.notEqual(thin, original);
  for (const color of ['#617de6','#e773d1','#65d2d2','#d4a73e']) assert.ok(thin.includes(color));
});
test('SVG export/import restores all values and rejects unsupported input', async () => {
  const {render, readParameters, DEFAULTS} = await import(modelUrl);
  const params = {...DEFAULTS, outerWidth: 11.5, snowWidth: 7, white: '#ffffff', waveDepth: 5};
  assert.deepEqual(readParameters(render(source, params)), params);
  assert.deepEqual(readParameters(source.replace(/<metadata id="vp-brand-parameters">.*?<\/metadata>/s, '')), DEFAULTS);
  assert.throws(() => render(source, {...params, outerWidth: -1}));
  assert.throws(() => render(source, {...params, white: '"><script/>'}));
  assert.throws(() => readParameters('<svg><metadata id="vp-brand-parameters">{"version":9}</metadata></svg>'));
});
test('each editable parameter changes the exported drawing', async () => {
  const {render, DEFAULTS} = await import(modelUrl);
  const strip = svg => svg.replace(/<metadata id="vp-brand-parameters">.*?<\/metadata>/s, '');
  const original = strip(render(source, DEFAULTS));
  for (const [key, value] of Object.entries({outerWidth:10,snowWidth:8,dividerTop:10,dividerBottom:8,sunWidth:8,waveDepth:4,white:'#ffffff',join:'bevel'})) {
    assert.notEqual(strip(render(source,{...DEFAULTS,[key]:value})),original,key);
  }
});
test('an exported SVG remains editable after becoming the next source', async () => {
  const {render,COLOR_FIRST,DEFAULTS} = await import(modelUrl);
  const saved=render(source,{...COLOR_FIRST,waveDepth:5});
  const reopened=render(saved,{...DEFAULTS,outerWidth:10,waveDepth:3});
  assert.match(reopened,/stroke-width="10" stroke-linejoin="round"/);
  assert.match(reopened,/C77 129 87 129 98.5 132S120 135 131 132/);
});
test('sun diameter and position can be changed without changing mountains', async () => {
  const {render,DEFAULTS,readParameters} = await import(modelUrl);
  const p={...DEFAULTS,sunDiameter:44,sunX:180,sunY:48};
  const svg=render(source,p);
  assert.match(svg,/<circle cx="180" cy="48" r="22"/);
  assert.deepEqual(readParameters(svg),p);
  assert.ok(svg.includes('M30 198L98 72L143 153L177 94L230 198Z'));
});
test('background gradient and halo are independently editable and round trip', async () => {
  const {render,DEFAULTS,readParameters} = await import(modelUrl);
  const p={...DEFAULTS,backgroundTop:'#203040',backgroundBottom:'#102030',glowColor:'#dd8844',glowStrength:50};
  const svg=render(source,p);
  assert.match(svg,/stop-color="#203040"/);
  assert.match(svg,/stop-color="#102030"/);
  assert.match(svg,/stop-color="#dd8844" stop-opacity="0.32"/);
  assert.match(svg,/stop-color="#dd8844" stop-opacity="0.13"/);
  assert.deepEqual(readParameters(svg),p);
  assert.throws(()=>render(source,{...p,glowColor:'bad'}));
});
test('halo center moves independently and survives export/re-edit', async () => {
  const {render,DEFAULTS,readParameters} = await import(modelUrl);
  const p={...DEFAULTS,glowX:72,glowY:65};
  const svg=render(source,p);
  assert.match(svg,/<radialGradient id="green-halo" cx="72%" cy="65%" r="72%"/);
  assert.deepEqual(readParameters(svg),p);
  const moved=render(svg,{...p,glowX:0,glowY:100});
  assert.match(moved,/cx="0%" cy="100%"/);
  assert.throws(()=>render(source,{...p,glowX:101}));
});
