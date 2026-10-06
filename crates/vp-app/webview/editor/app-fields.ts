import { useEditorHost, type EditorField } from '@chronista-club/creo-ui-editor-host'
import { onCleanup } from 'solid-js'

/** Default values remain in the stylesheet so changing a base theme can flow through. */
function cssField(id: string, label: string, cssVar: string, initial: string | number, group: string, constraints?: EditorField['constraints']): EditorField {
  return { id, label, cssVar, initial, group, constraints, type: typeof initial === 'number' ? 'number' : 'color', semantic: 'global', scope: 'token',
    apply: value => {
      if (value === initial) document.documentElement.style.removeProperty(cssVar)
    },
  }
}

export function appFields(): EditorField[] {
  const fields: EditorField[] = []
  const sizes = (prefix: string, group: string, specs: Array<[string, string, number, number, number]>) => {
    for (const [key, label, value, min, max] of specs) fields.push(cssField(`${prefix}.text.${key}`, label, `--${prefix}-text-${key}`, value, group, { min, max, step: .5, unit: 'px' }))
  }
  sizes('sb', '文字サイズ', [['base', '本文', 13, 10, 18], ['hint', '補足', 12, 9, 16], ['meta', 'メタ情報', 11, 8, 15], ['micro', '小さなラベル', 10, 7, 14]])
  const computed = getComputedStyle(document.documentElement)
  const colors: Array<[string, string, string, string, string]> = [
    ['sb.background', '背景', '--lg-void', '#05070A', '背景と文字'],
    ['sb.panel', 'メニュー・パネル', '--lg-panel', '#0A0E15', '背景と文字'],
    ['sb.text.primary', '本文の色', '--lg-hot', '#EAFBFF', '背景と文字'],
    ['sb.text.secondary', '補足の色', '--lg-mute', '#5C7A85', '背景と文字'],
    ['sb.text.tertiary', '控えめな文字', '--lg-mute-2', '#38525b', '背景と文字'],
    ['sb.border', '境界線', '--lg-hairline', '#12222b', '背景と文字'],
    ['sb.conn.auto', '操作のアクセント', '--sb-conn-auto', '#FFF76B', '背景と文字'],
    ['sb.selection.bg', 'Lane＋メイン背景', '--sb-selection-bg', '#151c27', '選択行'],
    ['sb.selection.border', '選択 Lane の輪郭色', '--sb-selection-border', '#68758b', '選択行'],
    ['sb.activity.thinking', '思考中', '--sb-activity-thinking', '#c3b4e8', '状態ポイント'],
    ['sb.activity.working', '作業中', '--sb-activity-working', '#a7c9ec', '状態ポイント'],
    ['sb.activity.waiting', '確認待ち', '--sb-activity-waiting', '#ef777d', '状態ポイント'],
    ['sb.activity.completed', '応答完了', '--sb-activity-completed', '#527ec6', '状態ポイント'],
    ['sb.activity.idle', '停止中', '--sb-activity-idle', '#8c98a5', '状態ポイント'],
    ['sb.activity.error', 'エラー', '--sb-activity-error', '#ef777d', '状態ポイント'],
  ]
  for (const [id, label, cssVar, fallback, group] of colors) fields.push(cssField(id, label, cssVar, computed.getPropertyValue(cssVar).trim() || fallback, group))
  fields.push(cssField('sb.selection.width', '選択 Lane の線幅', '--sb-selection-width', 1, '選択行', { min: 0, max: 3, step: .5, unit: 'px' }))
  sizes('chat', 'Chat の文字', [['body', '本文', 15, 12, 22], ['user', '自分のメッセージ', 13.5, 10, 20], ['tool', 'ツール・思考', 12, 9, 17], ['meta', 'メタ情報', 11, 8, 15], ['micro', '詳細ラベル', 10, 7, 14]])
  fields.push(cssField('shell.resizer.breathe.ms', '境界の明滅周期', '--vp-resizer-breathe-ms', 1694, '境界の動き', { min: 300, max: 2000, step: 1, unit: 'ms' }))
  fields.push(cssField('shell.resizer.breathe.dip', '境界の沈み込み', '--vp-resizer-breathe-dip', .775, '境界の動き', { min: .2, max: 1, step: .025 }))
  return fields
}

export function AppTokenBinds() {
  onCleanup(useEditorHost().register(appFields()))
  return null
}
