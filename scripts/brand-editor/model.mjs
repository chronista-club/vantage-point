// The canonical SVG remains the artwork source. Exported SVGs carry editable values.
export const DEFAULTS = Object.freeze({outerWidth:16,snowWidth:14,dividerTop:16,dividerBottom:12,sunWidth:14,waveDepth:9,white:'#edeef5',join:'round',sunDiameter:36,sunX:166,sunY:40,backgroundTop:'#242641',backgroundBottom:'#0a0826',glowColor:'#37b880',glowStrength:100,glowX:36,glowY:30});
export const COLOR_FIRST = Object.freeze({...DEFAULTS,outerWidth:12,snowWidth:8,dividerTop:12,dividerBottom:8,sunWidth:8});
export const CONTROLS = [
  ['outerWidth','山の外周',4,24,.5], ['snowWidth','雪の境界',2,20,.5],
  ['dividerTop','中央の仕切り · 上',2,24,.5], ['dividerBottom','中央の仕切り · 下',2,20,.5],
  ['sunWidth','太陽の輪郭',2,20,.5], ['waveDepth','雪の波の深さ',0,16,.5],
  ['sunDiameter','太陽の直径',8,64,.5], ['sunX','太陽の横位置',20,236,.5], ['sunY','太陽の縦位置',20,140,.5],
  ['glowStrength','背景の光の強さ',0,150,1],
  ['glowX','光の中心 · 横位置（%）',0,100,1], ['glowY','光の中心 · 縦位置（%）',0,100,1],
];
export const COLORS = [['white','線の色'],['backgroundTop','背景 · 左上'],['backgroundBottom','背景 · 右下'],['glowColor','背景の光の色']];
const metadataPattern = /<metadata id="vp-brand-parameters">([\s\S]*?)<\/metadata>/;
export function validate(values) {
  const result = {...DEFAULTS,...values};
  for (const key of Object.keys(result)) if (!(key in DEFAULTS)) throw new Error(`未知のパラメータ: ${key}`);
  for (const [key,,min,max] of CONTROLS) {
    if (typeof result[key] !== 'number' || !Number.isFinite(result[key]) || result[key]<min || result[key]>max) throw new Error(`範囲外: ${key} (${min}–${max})`);
  }
  for (const [key,label] of COLORS) if (!/^#[0-9a-f]{6}$/i.test(result[key])) throw new Error(`${label}は6桁のHEXを指定してください`);
  if (!['round','bevel','miter'].includes(result.join)) throw new Error('未知の角の形');
  return result;
}
export function readParameters(svg) {
  const match = svg.match(metadataPattern);
  if (!match) {
    if (!svg.includes('id="vp-mark"')) throw new Error('VPロゴのSVGではありません');
    return {...DEFAULTS};
  }
  const data = JSON.parse(match[1]);
  if (data.version !== 1 || !data.values || typeof data.values !== 'object' || Array.isArray(data.values)) throw new Error('対応していないパラメータ形式です');
  return validate(data.values);
}
const num = value => String(Number(value.toFixed(3)));
function divider(top, bottom) {
  // A tapered ribbon along the shared mountain edge. Smoothstep eases its first half.
  const length = Math.hypot(25,45), nx = -45/length, ny = 25/length;
  const side = sign => Array.from({length:49},(_,i) => {
    const t=i/48, u=Math.min(1,t*2), ease=u*u*(3-2*u);
    const half=(top+(bottom-top)*ease)/2;
    return `${num(143+25*t+sign*nx*half)} ${num(153+45*t+sign*ny*half)}`;
  });
  return `M${side(1).join('L')}L${side(-1).reverse().join('L')}Z`;
}
export function render(source, values = DEFAULTS) {
  const p=validate(values);
  const previous=readParameters(source);
  let svg=source.replace(metadataPattern,'');
  const mark=svg.match(/<g color="[^"]+" id="vp-mark">([\s\S]*?)<\/g>/);
  if (!mark) throw new Error('SVGの構造が変わりました。vp-markを確認してください');
  let body=mark[1];
  if (p.dividerTop!==previous.dividerTop || p.dividerBottom!==previous.dividerBottom) body=body.replace(/<path d="[^"]+" fill="currentColor"\s*\/>/,`<path d="${divider(p.dividerTop,p.dividerBottom)}" fill="currentColor" />`);
  body=body.replace(/stroke-width="[\d.]+" stroke-linecap="round"/,`stroke-width="${p.snowWidth}" stroke-linecap="round"`)
    .replace(/stroke-width="[\d.]+" stroke-linejoin="[a-z]+"/,`stroke-width="${p.outerWidth}" stroke-linejoin="${p.join}"`);
  body=body.replace(/(<circle[^>]+stroke-width=")[\d.]+("[^>]*\/?>)/,`$1${p.sunWidth}$2`);
  body=body.replace(/<circle cx="[\d.]+" cy="[\d.]+" r="[\d.]+"/,`<circle cx="${p.sunX}" cy="${p.sunY}" r="${p.sunDiameter/2}"`);
  if(p.waveDepth!==previous.waveDepth) {
    const high=num(132-p.waveDepth),low=num(132+p.waveDepth);
    body=body.replace(/C120 [\d.]+ 110 [\d.]+ 98.5 132S77 [\d.]+ 66 132/,`C120 ${low} 110 ${low} 98.5 132S77 ${high} 66 132`)
      .replace(/C77 [\d.]+ 87 [\d.]+ 98.5 132S120 [\d.]+ 131 132/,`C77 ${high} 87 ${high} 98.5 132S120 ${low} 131 132`);
  }
  svg=svg.replace(mark[0],`<g color="${p.white}" id="vp-mark">${body}</g>`);
  svg=svg.replace(/<linearGradient id="green-background"[\s\S]*?<\/linearGradient>/, gradient => gradient
    .replace(/(<stop offset="0%" stop-color=")[^"]+/,`$1${p.backgroundTop}`)
    .replace(/(<stop offset="100%" stop-color=")[^"]+/,`$1${p.backgroundBottom}`));
  svg=svg.replace(/<radialGradient id="green-halo"[\s\S]*?<\/radialGradient>/, gradient => gradient
    .replace(/cx="[\d.]+%"/,`cx="${p.glowX}%"`)
    .replace(/cy="[\d.]+%"/,`cy="${p.glowY}%"`)
    .replace(/stop-color="[^"]+"/g,`stop-color="${p.glowColor}"`)
    .replace(/(offset="0%"[^>]*stop-opacity=")[^"]+/,`$1${num(.64*p.glowStrength/100)}`)
    .replace(/(offset="55%"[^>]*stop-opacity=")[^"]+/,`$1${num(.26*p.glowStrength/100)}`));
  return svg.replace(/(<svg\b[^>]*>)/,`$1<metadata id="vp-brand-parameters">${JSON.stringify({version:1,values:p})}</metadata>`);
}
