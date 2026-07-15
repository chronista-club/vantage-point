/**
 * ResyncLoader — Act II 会話の「再同期・復帰中」を画面隅で知らせるコーナーローディング。
 *
 * trigger: active lane の transcript replay（attach / reconnect / demand 再発火で engine が
 * 過去会話を冪等再送する `replay_start`→`replay_end` の window、chatview.ts の `replaying`）。
 * その間だけ右下にインク状の blob アニメを出す（Switch/Splatoon の画面隅ローディングの質感を、
 * アセット流用なしで VP の accent color で再現）。
 *
 * 実装方針:
 * - 描画は ThreeJS（WebGL）。fullscreen quad の fragment shader で metaball を閾値化した
 *   ink-blob を描き、`uTime` uniform で swirl させる。
 * - 出入りは anime.js（v4、named export）。opacity + scale を elastic で pop-in / quad で pop-out。
 * - RAF は可視中のみ回す（imperative な描画ループと SolidJS reactive の接続は「canvas は一度だけ
 *   生成し、start/stop だけを signal で駆動」が定石。毎回 re-mount すると WebGL context 生成が重い）。
 * - `prefers-reduced-motion` 時は WebGL を張らず、CSS の緩い opacity pulse に fallback。
 */

import * as THREE from 'three'
import { animate } from 'animejs'
import { onMount, onCleanup, createEffect, createSignal } from 'solid-js'
import { render } from 'solid-js/web'
import { activeLaneReplaying } from './chatview'

/** canvas の論理サイズ（px）。DPR は renderer 側で掛ける。 */
const SIZE = 50

const VERT = /* glsl */ `
  varying vec2 vUv;
  void main() {
    vUv = uv;
    gl_Position = vec4(position.xy, 0.0, 1.0);
  }
`

// 六角形ローダー: hexagon SDF（iq 由来）で輪郭リングを描き、外周を回るコメット状の
// ハイライトで loading の回転を表す。内側は淡くグロー。accent color で着彩し canvas 端は溶かす。
const FRAG = /* glsl */ `
  precision mediump float;
  varying vec2 vUv;
  uniform float uTime;
  uniform vec3 uColor;

  // flat-top 六角形の signed distance（iq）。r = 中心→辺の距離。
  float sdHexagon(vec2 p, float r) {
    const vec3 k = vec3(-0.866025404, 0.5, 0.577350269);
    p = abs(p);
    p -= 2.0 * min(dot(k.xy, p), 0.0) * k.xy;
    p -= vec2(clamp(p.x, -k.z * r, k.z * r), r);
    return length(p) * sign(p.y);
  }

  mat2 rot(float a) { float c = cos(a), s = sin(a); return mat2(c, -s, s, c); }

  const float TAU = 6.2831853;

  void main() {
    vec2 p = (vUv - 0.5) * 2.0; // [-1, 1]
    float t = uTime;
    p = rot(t * 0.35) * p; // 六角形をゆっくり回す

    float R = 0.66 + 0.03 * sin(t * 0.5); // 呼吸するサイズ（明滅は控えめ・4x 遅く）
    float d = sdHexagon(p, R);

    // 塗りつぶし六角形（くっきり境界）+ 明るい縁 = 形が一目で分かる。
    float fillMask = smoothstep(0.012, -0.012, d);  // 内部 = 1（鋭い境界）
    float rim = smoothstep(0.055, 0.0, abs(d));      // 縁だけ明るく

    // 外周を回るコメット（loading の回転）: 角度距離でフェードした明点を縁に載せる。
    float ang = atan(p.y, p.x);
    float head = mod(t * 1.25, TAU);
    float da = abs(mod(ang - head + 3.14159265, TAU) - 3.14159265);
    float comet = smoothstep(1.2, 0.0, da) * rim;

    float alpha = clamp(fillMask * 0.42 + rim * 0.55, 0.0, 1.0);
    vec3 col = mix(uColor * 0.75, uColor, rim); // 内部は少し暗く、縁は accent 全開
    col += uColor * comet * 1.5;                 // コメット先端を白飛び気味に
    gl_FragColor = vec4(col, clamp(alpha + comet * 0.6, 0.0, 1.0));
  }
`

/**
 * アクティブ theme の accent（creoui `--color-brand-primary`）を THREE.Color にする。
 *
 * `--color-brand-primary` は theme ごとに **oklch** で宣言される。probe 要素の `color` に `var()`
 * 経由で当てて computed `color` を読むと、WebKit では `oklab(...)` に解決され THREE.Color は
 * これをパースできない（白のまま無反応）。そこで **canvas 2d の fillStyle → 1px 描画 → ピクセル
 * rgb 読み** でブラウザに rgb 正規化させる。canvas が解決色を解せない場合は先に敷いた青が残るので、
 * いずれにせよ安全に着色される（最悪 = VP 既定青、最良 = 現 theme の brand primary）。
 */
function readAccent(host: HTMLElement): THREE.Color {
  const fallback = '#3b82f6'
  try {
    const probe = document.createElement('span')
    probe.style.color = `var(--color-brand-primary, ${fallback})`
    probe.style.display = 'none'
    host.appendChild(probe)
    const resolved = getComputedStyle(probe).color
    host.removeChild(probe)

    const ctx = document.createElement('canvas').getContext('2d')
    if (!ctx) return new THREE.Color(fallback)
    ctx.fillStyle = fallback // 未対応 color string でも fillStyle が無視され青が残る保険
    ctx.fillStyle = resolved // 解せれば上書き（oklab/rgb どちらも WebKit は canvas で受ける）
    ctx.fillRect(0, 0, 1, 1)
    const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data
    return new THREE.Color(r / 255, g / 255, b / 255)
  } catch {
    return new THREE.Color(fallback)
  }
}

export function ResyncLoader() {
  let wrapEl!: HTMLDivElement
  let canvasEl!: HTMLCanvasElement
  let raf = 0
  let running = false
  let startMs = 0
  let renderer: THREE.WebGLRenderer | null = null
  let scene: THREE.Scene | null = null
  let camera: THREE.OrthographicCamera | null = null
  let material: THREE.ShaderMaterial | null = null

  const reduced = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false
  // WebGL が実際に初期化できたか（reduced or 失敗 = CSS pulse fallback）。ref は render 時点で
  // 未代入なので classList で直接 `!canvasEl` を読むと恒久 no-webgl になる罠を避け、signal で確定。
  const [webglOk, setWebglOk] = createSignal(false)

  const loop = (now: number) => {
    if (!running) return
    if (material) material.uniforms.uTime.value = (now - startMs) / 1000
    renderer?.render(scene!, camera!)
    raf = requestAnimationFrame(loop)
  }

  const startLoop = () => {
    if (running || reduced || !renderer) return
    running = true
    startMs = performance.now()
    raf = requestAnimationFrame(loop)
  }
  const stopLoop = () => {
    running = false
    cancelAnimationFrame(raf)
  }

  const show = () => {
    wrapEl.style.visibility = 'visible'
    startLoop()
    if (reduced) {
      wrapEl.style.opacity = '1'
      return
    }
    animate(wrapEl, { opacity: [0, 1], scale: [0.5, 1], duration: 480, ease: 'outElastic(1, .6)' })
  }
  const hide = () => {
    if (reduced) {
      wrapEl.style.opacity = '0'
      wrapEl.style.visibility = 'hidden'
      stopLoop()
      return
    }
    animate(wrapEl, {
      opacity: 0,
      scale: 0.5,
      duration: 240,
      ease: 'inQuad',
      onComplete: () => {
        wrapEl.style.visibility = 'hidden'
        stopLoop() // 非可視の間は RAF を止め、WebGL を遊ばせない
      },
    })
  }

  onMount(() => {
    wrapEl.style.opacity = '0'
    wrapEl.style.visibility = 'hidden'
    if (reduced) return // WebGL は張らず CSS pulse に委ねる

    try {
      renderer = new THREE.WebGLRenderer({ canvas: canvasEl, alpha: true, antialias: true })
      renderer.setClearColor(0x000000, 0)
      renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2))
      renderer.setSize(SIZE, SIZE, false)
      scene = new THREE.Scene()
      camera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1)
      material = new THREE.ShaderMaterial({
        transparent: true,
        depthTest: false,
        uniforms: {
          uTime: { value: 0 },
          uColor: { value: readAccent(wrapEl) },
        },
        vertexShader: VERT,
        fragmentShader: FRAG,
      })
      scene.add(new THREE.Mesh(new THREE.PlaneGeometry(2, 2), material))
      setWebglOk(true) // canvas を表示（no-webgl を外す）
    } catch {
      // WebGL 生成失敗（context 枯渇等）は CSS pulse fallback に degrade。
      renderer = null
    }
  })

  // active lane の replaying に追従して出入り。単一 signal 駆動なので canvas は再生成されない。
  createEffect(() => (activeLaneReplaying() ? show() : hide()))

  onCleanup(() => {
    stopLoop()
    material?.dispose()
    renderer?.dispose()
  })

  return (
    <div ref={wrapEl} class="vp-resync" classList={{ 'no-webgl': reduced || !webglOk() }}>
      <canvas ref={canvasEl} class="vp-resync-canvas" />
      <span class="vp-resync-dot" />
      <span class="vp-resync-label">再同期中</span>
    </div>
  )
}

/** body 直下に host を作って ResyncLoader を mount する（entry.tsx から呼ぶ）。 */
export function mountResyncLoader(): void {
  const host = document.createElement('div')
  host.id = 'vp-resync-host'
  document.body.appendChild(host)
  render(() => <ResyncLoader />, host)
}

/** entry.tsx から head 注入する CSS。 */
export const RESYNC_LOADER_CSS = /* css */ `
.vp-resync {
  position: fixed; right: 20px; top: 56px; z-index: 9990;
  width: ${SIZE}px; height: ${SIZE}px; pointer-events: none;
  display: flex; align-items: center; justify-content: center;
  transform-origin: top right; will-change: transform, opacity;
}
.vp-resync-canvas { width: ${SIZE}px; height: ${SIZE}px; display: block; }
/* reduced-motion / WebGL 不可時の fallback: accent の緩い opacity pulse（motion 最小）。 */
.vp-resync-dot { display: none; }
.vp-resync.no-webgl .vp-resync-canvas { display: none; }
.vp-resync.no-webgl .vp-resync-dot {
  display: block; width: 42px; height: 42px; border-radius: 50%;
  background: radial-gradient(circle, var(--color-accent, #3b82f6), transparent 70%);
  animation: vp-resync-pulse 1.3s ease-in-out infinite;
}
.vp-resync-label {
  position: absolute; bottom: -3px; left: 50%; transform: translateX(-50%);
  font-size: 9px; letter-spacing: 0.08em; white-space: nowrap;
  color: var(--color-text-tertiary, #8b93a7);
  font-family: var(--vp-font-mono), var(--typography-family-mono), monospace;
  text-shadow: 0 1px 3px rgba(0, 0, 0, 0.6);
}
@keyframes vp-resync-pulse {
  0%, 100% { opacity: 0.35; }
  50% { opacity: 1; }
}
`
