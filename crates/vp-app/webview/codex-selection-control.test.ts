// @vitest-environment happy-dom
import { expect, it } from 'vitest'
import { changeCodexSelection } from './codex-selection-control'

it('保存拒否後も表示は確定したモデルと一致する', async () => {
  const select = document.createElement('select')
  select.innerHTML = '<option value="old">Old</option><option value="new">New</option>'
  select.value = 'new'
  let requested = ''
  changeCodexSelection(select, 'old', value => { requested = value })
  expect(requested).toBe('new')
  await Promise.resolve()
  expect(select.value).toBe('old')
})
