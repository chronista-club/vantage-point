/**
 * HistoryStrip.tsx の pure function unit tests
 *
 * SolidJS component 本体は DOM 依存のため Medium/Large 領域だが、
 * `truncate` / `aliasOf` は DOM 非依存の pure calculation。
 * これらを Small Test として集中的に検証する。
 */

import { describe, expect, it } from 'vitest'
import { aliasOf, truncate } from './HistoryStrip'
import type { CanvasItem } from './canvas-handler'

// ============================================================================
// truncate — codepoint ベースの文字列切り詰め (doc 19 §5.1)
// ============================================================================

describe('truncate', () => {
  it('正常系: n 文字以内の文字列はそのまま返す', () => {
    expect(truncate('hello', 8)).toBe('hello')
    expect(truncate('12345678', 8)).toBe('12345678')
  })

  it('境界値: ちょうど n 文字は truncate されない', () => {
    expect(truncate('abcdefgh', 8)).toBe('abcdefgh')
  })

  it('正常系: n+1 文字以上は n 文字 + "…" になる', () => {
    expect(truncate('abcdefghi', 8)).toBe('abcdefgh…')
    expect(truncate('hello world', 8)).toBe('hello wo…')
  })

  it('正常系: n=0 は常に "…" (空文字の場合は空)', () => {
    expect(truncate('a', 0)).toBe('…')
    expect(truncate('', 0)).toBe('')
  })

  it('正常系: マルチバイト文字 (日本語) も codepoint 単位でカウントされる', () => {
    // "あいうえおかきく" = 8 codepoints → そのまま
    expect(truncate('あいうえおかきく', 8)).toBe('あいうえおかきく')
    // 9 文字 → 8 + "…"
    expect(truncate('あいうえおかきくけ', 8)).toBe('あいうえおかきく…')
  })

  it('正常系: 絵文字も codepoint 単位でカウントされる', () => {
    // 絵文字は通常 2 codepoint (surrogate pair) だが spread で 1 unit — 実装依存
    // 「8 文字以内なら切らない」ことを確認
    const emoji = '🎉🎉🎉'
    const result = truncate(emoji, 8)
    // 3 絵文字 <= 8 なのでそのまま (絵文字が 1 codepoint に見える場合)
    // 実装の spread [...s] は surrogate pair を 1 unit として扱う
    expect(result).toBe(emoji)
  })
})

// ============================================================================
// aliasOf — title / content fallback 解決 (doc 19 §5.1)
// ============================================================================

function makeItem(partial: Partial<CanvasItem> & { content: string }): CanvasItem {
  return {
    id: 'test-id',
    contentType: 'markdown',
    createdAt: '',
    ...partial,
  }
}

describe('aliasOf', () => {
  it('正常系: title が存在する場合は title を返す', () => {
    const item = makeItem({ content: 'body text', title: 'My Title' })
    expect(aliasOf(item)).toBe('My Title')
  })

  it('正常系: title が空文字の場合は content 先頭行を返す', () => {
    const item = makeItem({ content: 'first line\nsecond line', title: '' })
    expect(aliasOf(item)).toBe('first line')
  })

  it('正常系: title が空白のみの場合も content 先頭行を返す', () => {
    const item = makeItem({ content: 'first line', title: '   ' })
    expect(aliasOf(item)).toBe('first line')
  })

  it('正常系: title undefined の場合は content 先頭行を返す', () => {
    const item = makeItem({ content: '# Heading\nbody' })
    expect(aliasOf(item)).toBe('# Heading')
  })

  it('正常系: content が空の場合は "(empty)" を返す', () => {
    const item = makeItem({ content: '' })
    expect(aliasOf(item)).toBe('(empty)')
  })

  it('正常系: content が空白のみの場合も "(empty)" を返す', () => {
    const item = makeItem({ content: '   \n  \n  ' })
    expect(aliasOf(item)).toBe('(empty)')
  })

  it('正常系: content の先頭に空行がある場合は最初の非空行を返す', () => {
    // trim() してから split なので先頭の空白は除かれる
    const item = makeItem({ content: '\n\nactual content' })
    expect(aliasOf(item)).toBe('actual content')
  })

  it('正常系: title の前後の空白は trim される', () => {
    const item = makeItem({ content: 'body', title: '  My Title  ' })
    expect(aliasOf(item)).toBe('My Title')
  })
})
