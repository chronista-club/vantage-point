/**
 * EchoesHeader — Act I/II 共通の lane-local context strip（creo memo `vp-pane-common-header`）。
 *
 * pane-host 上端の `#echoes-header`（main_area.rs が mount 点だけ提供）に SolidJS で mount され、
 * どの PaneKind / Act を表示していても「lane の Echoes」の情報が載り続ける
 * （帰属は PaneKind ではなく lane の Echoes —「Echoes は一つ、視え方が二つ」の UI 体現）。
 * chips は presence-driven — 値があるものだけ並ぶ。
 *
 * データ源（既存経路への相乗りのみ、新しい Rust→JS 配信チャネルは作らない）:
 * - lane 名 / cwd / branch / Act 初期値: setActivePane payload（entry.tsx bridge が setLane で届ける）
 * - cc session id / permission mode / engine 途絶: vpConsole の header summary
 *   （session_init / turn_completed / error の畳み込み。'vp:echoes-header' event で追従）
 * - Act 切替追従: 'vp:console-mode' event（vpConsole.setMode が dispatch する既存 bus）
 *
 * 操作エリア（右側）:
 * - interrupt（chat 時のみ、既存 IPC `echoes:interrupt`）
 * - Act toggle / New Session は entry.tsx の既存 imperative ボタン群が
 *   `#echoes-header-actions`（本 component が描く安定 div）に append される（移設、挙動は不変）。
 */

import { render } from 'solid-js/web'
import { createSignal, For, Show } from 'solid-js'
import type { VpConsole, EchoesHeaderState, EchoesSession } from './console'

// ---------------------------------------------------------------------------
// 純関数（vitest 対象）
// ---------------------------------------------------------------------------

/** $HOME 相当 prefix を ~ に畳む（Mac 主軸: /Users/<u>。Linux の /home/<u> も受ける）。 */
export function tildify(path: string): string {
  return path.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, '~')
}

/** 中略: maxLen 超過時に頭を残し末尾を厚めに取って '…' で繋ぐ（path は末尾ほど情報が濃い）。 */
export function middleEllipsis(s: string, maxLen = 42): string {
  if (s.length <= maxLen) return s
  const tail = Math.ceil((maxLen - 1) * 0.6)
  const head = maxLen - 1 - tail
  return `${s.slice(0, head)}…${s.slice(s.length - tail)}`
}

/**
 * LaneAddress::Display 形から表示用短名を導く。
 * `<project>/conductor` → "conductor"、`<project>/performer/<name>` → name。
 * legacy `lead` / `wing` も受理（entry.tsx laneNameFromAddress と同じ語彙）。
 */
export function laneShortName(addr: string): string {
  if (addr.endsWith('/conductor') || addr.endsWith('/lead')) return 'conductor'
  const m = addr.match(/\/(?:performer|wing)\/(.+)$/)
  return m?.[1] ?? addr
}

/** permission mode の chip 表示名（長い canonical 名だけ縮める、未知値は素通し）。 */
export function permModeLabel(mode: string): string {
  return mode === 'bypassPermissions' ? 'bypass' : mode
}

/**
 * session chip の engine 別 prefix（doc 37: engine = session 束縛の identity なので、
 * chip 自体が「どの engine の session か」を兼ねる）。
 * claude は歴史的な `cc`（Claude Code）を維持、未知 engine（撤去済み cursor / agy 含む）は
 * 中立の `sid`。
 */
export function sessionChipPrefix(stand: string | null | undefined): string {
  switch (stand) {
    case 'echoes':
    case 'hd':
      return 'cc'
    case 'codex':
      return 'cdx'
    case 'grok':
      return 'grok'
    case 'opencode':
      return 'oc'
    default:
      return 'sid'
  }
}

/** Root 切替 picker の 1 行（doc 39 P3 → P4）。 */
export type RootPickerItem = {
  key: number
  /** `cc:3d91933b` 形。会話 id が未発行（Draft / 未発話）の session は `cc:新品`。 */
  label: string
  isRoot: boolean
  /** engine が未知（chip prefix が `sid` = 撤去済み / legacy stand）= 切替不可。 */
  disabled: boolean
}

/**
 * echoes_session_list の sessions を picker の表示行へ畳む（doc 39 P3 → P4 — Root 切替 picker）。
 * 並びは SP の登録順そのまま（key 昇順 = 生成順、tab strip と同じ秩序）。全 session を列挙する。
 * doc 39 P4: slot の respawn が root session の stand で engine を決めるようになった（P4-A）ため、
 * cross-engine の Root 切替が解禁された。disabled にするのは **engine が未知**（chip prefix が
 * `sid` = 撤去済み cursor/agy や legacy stand — shell 層に落ちて resume が効かない）行のみ。
 * backend の `prepare_switch_root_session`（未知 engine を Err）と二重防御。
 */
export function rootPickerItems(sessions: EchoesSession[]): RootPickerItem[] {
  return sessions.map((s) => ({
    key: s.key,
    label: `${sessionChipPrefix(s.stand)}:${s.engine_session_id ? s.engine_session_id.slice(0, 8) : '新品'}`,
    isRoot: s.root === true,
    disabled: sessionChipPrefix(s.stand) === 'sid',
  }))
}

// ---------------------------------------------------------------------------
// component
// ---------------------------------------------------------------------------

/** setActivePane（Rust push_active_view）が運ぶ lane 文脈。null = lane 未選択。 */
export type HeaderLaneCtx = {
  addr: string
  /** LaneInfo.name（無ければ addr から laneShortName で導出） */
  name: string | null
  cwd: string | null
  branch: string | null
  /** console_mode == "chat"（Act の初期値。以降は vp:console-mode で追従） */
  chat: boolean
  /** active engine の session id（LaneInfo.engine_session_id 由来）。
   *  Act I は EchoesEvent が流れないため、この供給路が無いと chip が出ない
   *  （bug mem_1Cd3icsvKiGsQ8TtX8t1FR）。Act II では event 由来の真値が優先される。 */
  sessionId: string | null
  /** root session の stand（= slot に載る engine 種別）。session chip の prefix 導出用。
   *  doc 39 P4-C: Rust push_active_view が engine_stand（root の engine）優先で解決した値
   *  （cross-engine root でも chip が slot の engine を映す。無ければ lane 固定 stand）。 */
  stand: string | null
}

export type EchoesHeaderApi = {
  setLane(ctx: HeaderLaneCtx | null): void
}

function copyText(text: string): void {
  const fallback = () => {
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify({ t: 'copy', d: text }))
  }
  try {
    // webview の permission policy で silent fail することがあるため IPC fallback 併用
    //（main_area.rs doCopy と同じ二段構え）。
    navigator.clipboard.writeText(text).catch(fallback)
  } catch {
    fallback()
  }
}

export function mountEchoesHeader(mount: HTMLElement, vpConsole: VpConsole): EchoesHeaderApi {
  const [ctx, setCtx] = createSignal<HeaderLaneCtx | null>(null)
  const [mode, setMode] = createSignal<'tui' | 'chat'>('tui')
  const [summary, setSummary] = createSignal<EchoesHeaderState>({})
  /** click copy の視覚 feedback（copied class を短時間付ける対象 chip の key）。 */
  const [copiedKey, setCopiedKey] = createSignal<string | null>(null)
  let copiedTimer: number | undefined

  const copy = (key: string, text: string): void => {
    copyText(text)
    setCopiedKey(key)
    if (copiedTimer) clearTimeout(copiedTimer)
    copiedTimer = window.setTimeout(() => setCopiedKey(null), 900)
  }

  // doc 39 P3: Root 切替 picker（Act I の chip click で開く dropdown）。
  // sessions は 'vp:echoes-sessions'（handleSessionList の既存 bus）から受ける — 新配信路は作らない。
  const [pickerOpen, setPickerOpen] = createSignal(false)
  const [pickerPos, setPickerPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 })
  const [sessions, setSessions] = createSignal<EchoesSession[]>([])

  const sendIpc = (payload: Record<string, unknown>): void => {
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify(payload))
  }

  /** chip click（Act I）: picker を開き、一覧を SP から取り直す（開くたび authoritative）。 */
  const openPicker = (chip: HTMLElement): void => {
    const lane = ctx()?.addr
    if (!lane) return
    const rect = chip.getBoundingClientRect()
    setPickerPos({ x: rect.left, y: rect.bottom + 4 })
    setPickerOpen(true)
    sendIpc({ t: 'echoes:sessions_fetch', lane })
  }

  /** picker 行 click: 既存 session へ root を向け替え（backend が slot を Resume respawn）。 */
  const switchRoot = (key: number): void => {
    const lane = ctx()?.addr
    setPickerOpen(false)
    if (!lane) return
    sendIpc({ t: 'console:switch_root', lane, session: key })
  }

  /** 「✨ 新 ID から」: 既存の New（Act I = new_root）に委譲する。 */
  const newRoot = (): void => {
    const lane = ctx()?.addr
    setPickerOpen(false)
    if (!lane) return
    sendIpc({ t: 'console:new_session', lane })
  }

  // 外側 click で閉じる（picker / chip の内側は除外 — chip click は toggle が担う）。
  document.addEventListener('click', (ev) => {
    if (!pickerOpen()) return
    const target = ev.target as HTMLElement | null
    if (target?.closest('.eh-root-picker, .eh-session')) return
    setPickerOpen(false)
  })

  // Act 切替追従（vpConsole.setMode → CustomEvent。表示中 lane 以外は無視）。
  document.addEventListener('vp:console-mode', (e) => {
    const d = (e as CustomEvent<{ lane: string; mode: 'tui' | 'chat' }>).detail
    if (d?.lane && d.lane === ctx()?.addr) setMode(d.mode)
  })
  // session 一覧追従（handleSessionList の既存 bus。picker が閉じていても cache しておく）。
  document.addEventListener('vp:echoes-sessions', (e) => {
    const d = (e as CustomEvent<{ lane: string; sessions?: EchoesSession[] }>).detail
    if (d?.lane && d.lane === ctx()?.addr) setSessions(d.sessions ?? [])
  })
  // session summary 追従（console.ts の畳み込みが変化した時だけ飛ぶ低頻度 event）。
  document.addEventListener('vp:echoes-header', (e) => {
    const d = (e as CustomEvent<{ lane: string }>).detail
    if (d?.lane && d.lane === ctx()?.addr) setSummary(vpConsole.headerState(d.lane))
  })

  const interrupt = (): void => {
    const lane = ctx()?.addr
    if (!lane) return
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify({ t: 'echoes:interrupt', lane }))
  }

  const Header = () => (
    <div class="eh-root" classList={{ 'eh-empty': !ctx() }}>
      <Show when={ctx()}>
        {(c) => (
          <>
            <span class="eh-chip eh-lane" title={c().addr}>
              💬 {c().name ?? laneShortName(c().addr)}
            </span>
            <span class="eh-chip eh-act">{mode() === 'chat' ? 'Act II' : 'Act I'}</span>
            <Show when={c().cwd}>
              <button
                type="button"
                class="eh-chip eh-cwd"
                classList={{ copied: copiedKey() === 'cwd' }}
                title={`${c().cwd}（click で full path を copy）`}
                onClick={() => copy('cwd', c().cwd!)}
              >
                {middleEllipsis(tildify(c().cwd!))}
              </button>
            </Show>
            <Show when={c().branch}>
              <span class="eh-chip eh-branch" title="git branch">
                ⎇ {c().branch}
              </span>
            </Show>
            {/* session chip: Act II は event 由来（summary）が真値、Act I は setActivePane
                相乗りの engine_session_id（ctx）が唯一の供給路 — OR merge で両 Act に出す。
                prefix は engine 別（cc/cdx/grok/oc、doc 37: chip が engine indicator を兼ねる）。
                click は Act で分岐（doc 39 P3、2026-07-18 mako 決定「表示器 = 操作器」）:
                Act I = Root 切替 picker を開く（copy は picker 内の行へ移設）/ Act II = 従来 copy
                （session 選択は tab strip が担うため picker は出さない。backend の Tui gate とも一致）。 */}
            <Show when={summary().sessionId ?? c().sessionId}>
              {(sid) => (
                <button
                  type="button"
                  class="eh-chip eh-session"
                  classList={{ copied: copiedKey() === 'sid' }}
                  title={
                    mode() === 'tui'
                      ? `${sessionChipPrefix(c().stand)} session ${sid()}（click で Root 切替）`
                      : `${sessionChipPrefix(c().stand)} session ${sid()}（click で copy）`
                  }
                  onClick={(ev) => {
                    if (mode() !== 'tui') {
                      copy('sid', sid())
                    } else if (pickerOpen()) {
                      setPickerOpen(false)
                    } else {
                      openPicker(ev.currentTarget as HTMLElement)
                    }
                  }}
                >
                  {sessionChipPrefix(c().stand)}:{sid().slice(0, 8)}
                  <Show when={mode() === 'tui'}>
                    <span class="eh-session-caret">▾</span>
                  </Show>
                </button>
              )}
            </Show>
            {/* doc 39 P3: Root 切替 picker。ヘッダ strip は overflow:hidden なので
                position:fixed で clipping の外に出す（開いた時点の chip 位置に固定）。 */}
            <Show when={pickerOpen()}>
              <div
                class="eh-root-picker"
                style={{ left: `${pickerPos().x}px`, top: `${pickerPos().y}px` }}
              >
                <div class="eh-rp-title">Root agent</div>
                <For
                  each={rootPickerItems(sessions())}
                  fallback={<div class="eh-rp-empty">読み込み中…</div>}
                >
                  {(item) => (
                    <button
                      type="button"
                      class="eh-rp-row"
                      classList={{ 'eh-rp-root': item.isRoot, 'eh-rp-disabled': item.disabled }}
                      title={
                        item.isRoot
                          ? '今の slot（root）'
                          : item.disabled
                            ? 'engine が未知のため切替不可'
                            : 'この session を root にする（slot を resume で張り替え）'
                      }
                      onClick={() => {
                        if (item.disabled) return
                        if (item.isRoot) setPickerOpen(false)
                        else switchRoot(item.key)
                      }}
                    >
                      {item.isRoot ? '●' : '○'} {item.label}
                      <span class="eh-rp-key">#{item.key}</span>
                      <Show when={item.isRoot}>
                        <span class="eh-rp-now">今の slot</span>
                      </Show>
                    </button>
                  )}
                </For>
                <div class="eh-rp-divider" />
                <button type="button" class="eh-rp-row" onClick={newRoot}>
                  ✨ 新 ID から（素の engine）
                </button>
                <Show when={summary().sessionId ?? c().sessionId}>
                  {(sid) => (
                    <button
                      type="button"
                      class="eh-rp-row"
                      onClick={() => {
                        copy('sid', sid())
                        setPickerOpen(false)
                      }}
                    >
                      ⧉ 今の id を copy
                    </button>
                  )}
                </Show>
              </div>
            </Show>
            <Show when={summary().permissionMode}>
              <span class="eh-chip eh-perm" title="permission mode">
                {permModeLabel(summary().permissionMode!)}
              </span>
            </Show>
            {/* engine 状態は Act II 表示時のみ（Act I は engine-less が正常形なので出さない）。
                本物の異常（⚠ engine）と回復可能な休眠（💤 休眠）を分けて出す。error 優先。 */}
            <Show when={mode() === 'chat' && summary().engineError}>
              <span class="eh-chip eh-engine-down" title={summary().engineError}>
                ⚠ engine
              </span>
            </Show>
            <Show when={mode() === 'chat' && !summary().engineError && summary().engineDormant}>
              <span class="eh-chip eh-engine-dormant" title={summary().engineDormant}>
                💤 休眠
              </span>
            </Show>
          </>
        )}
      </Show>
      <div class="eh-spacer" />
      <div class="eh-actions">
        <Show when={ctx() && mode() === 'chat'}>
          <button
            type="button"
            class="eh-chip eh-stop"
            onClick={interrupt}
            title="実行中 turn を中断（echoes:interrupt）"
          >
            ⏹ Stop
          </button>
        </Show>
        {/* entry.tsx の既存 Act toggle / New Session（imperative 生成）が append される安定 div。
            動的 JSX を含まないので Solid の再描画がここの子を触ることはない。 */}
        <div class="eh-legacy-actions" id="echoes-header-actions" />
      </div>
    </div>
  )

  render(() => <Header />, mount)

  return {
    setLane(next) {
      setCtx(next)
      setMode(next?.chat ? 'chat' : 'tui')
      setSummary(next ? vpConsole.headerState(next.addr) : {})
      // lane が替わったら picker は閉じ、一覧も捨てる（別 lane の session を誤表示しない）。
      setPickerOpen(false)
      setSessions([])
      // strip の開閉（World A の DOM に触れる唯一の接点）: lane 文脈がある時だけ
      // #pane-terminal に .echoes-header-active を付け、main_area.rs の --echoes-header-h を
      // 0→30px にして strip を開く。無い時は高さ 0 = xterm/chat が全面（regression なし）。
      // これが無いと strip は #echoes-header の overflow:hidden で永遠に高さ 0 = 不可視になる。
      document
        .getElementById('pane-terminal')
        ?.classList.toggle('echoes-header-active', !!next)
    },
  }
}

// ---------------------------------------------------------------------------
// CSS（entry.tsx が <style> で注入 — CHATVIEW_CSS と同 pattern）
// ---------------------------------------------------------------------------

export const ECHOES_HEADER_CSS = `
/* Echoes 共通ヘッダ strip。1 行・控えめ・creo tokens 準拠。strip 自体は window drag 面、
   chip / button は no-drag（.pane-header と同じ規約）。 */
#echoes-header .eh-root{ display:flex; align-items:center; gap:6px; height:30px; padding:0 10px;
  font-size:11.5px; background:var(--color-surface-surface);
  border-bottom:1px solid var(--color-surface-border-subtle);
  color:var(--color-text-secondary);
  font-family:var(--vp-font-sans),var(--typography-family-sans); font-weight:300;
  user-select:none; -webkit-app-region:drag; overflow:hidden; white-space:nowrap; }
#echoes-header .eh-chip{ -webkit-app-region:no-drag; display:inline-flex; align-items:center; gap:4px;
  flex:none; padding:1px 8px; border-radius:9999px; background:transparent;
  border:1px solid var(--color-surface-border-subtle); color:var(--color-text-secondary);
  font:inherit; line-height:1.6; max-width:36ch; overflow:hidden; text-overflow:ellipsis; }
#echoes-header button.eh-chip{ cursor:pointer; }
#echoes-header button.eh-chip:hover{ background:var(--color-surface-bg-emphasis); color:var(--color-text-primary); }
#echoes-header .eh-chip.copied{ border-color:var(--color-brand-primary); color:var(--color-text-primary); }
#echoes-header .eh-lane{ color:var(--color-text-primary); border-color:transparent; padding-left:0; font-weight:500; }
#echoes-header .eh-cwd, #echoes-header .eh-session{
  font-family:var(--vp-font-mono),var(--typography-family-mono); font-size:10.5px; }
#echoes-header .eh-engine-down{ color:#f0a3a3; border-color:rgba(240,163,163,.4); }
/* 休眠は異常ではないので muted（警告色にしない）。送信で起きることを title で伝える。 */
#echoes-header .eh-engine-dormant{ color:var(--color-text-tertiary); border-color:var(--color-border-subtle); opacity:.75; }
#echoes-header .eh-stop{ color:#f0a3a3; }
#echoes-header .eh-spacer{ flex:1; min-width:8px; }
#echoes-header .eh-actions{ display:flex; align-items:center; gap:8px; -webkit-app-region:no-drag; }
#echoes-header .eh-root.eh-empty .eh-actions{ display:none; }
#echoes-header .eh-legacy-actions{ display:flex; gap:8px; }
/* 移設された既存 Act toggle / New Session（CHATVIEW_CSS は絶対配置で定義）を strip 内 flow に戻す。 */
#echoes-header .echoes-console-actions{ position:static; top:auto; right:auto; }
/* doc 39 P3: Root 切替 picker。strip の overflow:hidden を position:fixed で脱出する。 */
#echoes-header .eh-session-caret{ opacity:.6; margin-left:2px; }
#echoes-header .eh-root-picker{ position:fixed; z-index:1000; min-width:230px; padding:4px;
  background:var(--color-surface-surface); border:1px solid var(--color-surface-border-subtle);
  border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.35); -webkit-app-region:no-drag;
  font-family:var(--vp-font-sans),var(--typography-family-sans); }
#echoes-header .eh-rp-title{ padding:4px 8px 2px; color:var(--color-text-secondary); font-size:10.5px; }
#echoes-header .eh-rp-row{ display:flex; align-items:center; gap:6px; width:100%; text-align:left;
  padding:5px 8px; border:0; border-radius:6px; background:transparent; cursor:pointer;
  color:var(--color-text-primary);
  font-family:var(--vp-font-mono),var(--typography-family-mono); font-size:10.5px; line-height:1.6; }
#echoes-header .eh-rp-row:hover{ background:var(--color-surface-bg-emphasis); }
#echoes-header .eh-rp-key{ color:var(--color-text-secondary); }
#echoes-header .eh-rp-row.eh-rp-root{ color:var(--color-brand-primary); }
#echoes-header .eh-rp-row.eh-rp-disabled{ opacity:.45; cursor:default; }
#echoes-header .eh-rp-row.eh-rp-disabled:hover{ background:transparent; }
#echoes-header .eh-rp-now{ margin-left:auto; color:var(--color-text-secondary); font-size:10px; }
#echoes-header .eh-rp-divider{ height:1px; margin:4px 6px; background:var(--color-surface-border-subtle); }
#echoes-header .eh-rp-empty{ padding:6px 8px; color:var(--color-text-secondary); font-size:10.5px; }
`
