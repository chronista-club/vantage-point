import {DEFAULTS,COLOR_FIRST,CONTROLS,COLORS,render,readParameters,validate} from './model.mjs';
const $ = id => document.getElementById(id);
let source, canonicalParameters={...DEFAULTS}, parameters={...COLOR_FIRST};
const status = (text,error=false) => { $('status').textContent=text; $('status').classList.toggle('error',error); };
const dataUrl = svg => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
function refresh() {
  const svg=render(source,parameters), url=dataUrl(svg);
  $('edited').src=url;
  for(const img of $('sizes').querySelectorAll('img')) img.src=url;
  const mark=svg.match(/<g color="[^"]+" id="vp-mark">[\s\S]*?<\/g>/)[0];
  $('mark').src=dataUrl(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">${mark}</svg>`);
  for(const [key] of CONTROLS) for(const input of document.querySelectorAll(`[data-key="${key}"]`)) input.value=parameters[key];
  $('join').value=parameters.join; for(const [key] of COLORS) $(key).value=parameters[key];
  try { sessionStorage.setItem('vp-brand-draft-v1',JSON.stringify(parameters)); } catch {}
  $('color-first').classList.toggle('selected',JSON.stringify(parameters)===JSON.stringify(COLOR_FIRST));
  $('reset').classList.toggle('selected',JSON.stringify(parameters)===JSON.stringify(canonicalParameters));
}
function update(key,value) {
  try { parameters=validate({...parameters,[key]:value}); refresh(); status('調整中 · SVGを保存すると値も残ります'); }
  catch(error) { status(error.message,true); }
}
for(const [key,label,min,max,step] of CONTROLS) {
  if(key==='sunDiameter' || key==='glowStrength') { const heading=document.createElement('h3');heading.textContent=key==='sunDiameter'?'太陽のサイズと位置':'背景の光';$('controls').append(heading); }
  const row=document.createElement('div'); row.className='control';
  row.innerHTML=`<div class="control-top"><label for="${key}">${label}</label><input aria-label="${label}の数値" data-key="${key}" type="number" min="${min}" max="${max}" step="${step}"></div><input id="${key}" aria-label="${label}" data-key="${key}" type="range" min="${min}" max="${max}" step="${step}">`;
  for(const input of row.querySelectorAll('input')) input.addEventListener('input',()=> { if(input.value!=='') update(key,Number(input.value)); });
  $('controls').append(row);
}
for(const size of [16,32,64,128]) {
  const figure=document.createElement('figure'); figure.innerHTML=`<img width="${size}" height="${size}" alt="${size}pxの調整中アイコン"><figcaption>${size}px</figcaption>`; $('sizes').append(figure);
}
$('join').addEventListener('change',e=>update('join',e.target.value));
for(const [key,label] of COLORS) {
  if(key!=='white') { const row=document.createElement('label');row.className='select-row';row.htmlFor=key;row.textContent=label;const input=document.createElement('input');input.type='color';input.id=key;row.append(input);$('colors').append(row); }
  $(key).addEventListener('input',e=>update(key,e.target.value));
}
$('color-first').addEventListener('click',()=> {parameters={...COLOR_FIRST};refresh();status('色面を広く見せる試案 · 外周12 / 雪8 / 太陽8');});
$('reset').addEventListener('click',()=> {parameters={...canonicalParameters};refresh();status('採用中のアイコンに戻しました');});
$('import').addEventListener('click',()=> $('file').click());
$('file').addEventListener('change',async e=> {
  const file=e.target.files[0]; if(!file)return;
  try { parameters=readParameters(await file.text());refresh();status(`${file.name} の調整値を読み込みました`); }
  catch(error){status(error.message,true);} finally{e.target.value='';}
});
$('export').addEventListener('click',()=> {
  const url=URL.createObjectURL(new Blob([render(source,parameters)],{type:'image/svg+xml'}));
  const a=document.createElement('a');a.href=url;a.download='vantage-point-parametric.svg';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
  status('SVGを書き出しました · 再び開いて続きから編集できます');
});
try {
  const response=await fetch('../../assets/brand/source.svg');if(!response.ok)throw new Error(`SVGの読み込みに失敗 (${response.status})`);
  source=await response.text();canonicalParameters=readParameters(source);parameters={...canonicalParameters};
  const licenseResponse=await fetch('../../assets/brand/PHOSPHOR-LICENSE.txt');
  if(!licenseResponse.ok)throw new Error('ライセンスの読み込みに失敗しました');
  const license=(await licenseResponse.text()).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;');
  if(!source.includes('id="vp-brand-license"'))source=source.replace(/(<svg\b[^>]*>)/,`$1<metadata id="vp-brand-license">${license}</metadata>`);
  const draft=location.hash.startsWith('#params=')?decodeURIComponent(location.hash.slice(8)):sessionStorage.getItem('vp-brand-draft-v1');
  if(draft) {try {parameters=validate(JSON.parse(draft));}catch{parameters={...COLOR_FIRST};}}
  if(location.hash.startsWith('#params='))history.replaceState(null,'',location.pathname+location.search);
  $('original').src=dataUrl(source);refresh();$('export').disabled=false;
  status(draft?'調整値を引き継ぎました':'採用中のアイコンから開始');
}catch(error){status(error.message,true);for(const input of document.querySelectorAll('input,button,select'))input.disabled=true;}
