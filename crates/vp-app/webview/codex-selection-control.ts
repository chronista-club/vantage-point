/** ブラウザ上の仮選択を、保存結果が届くまで確定値へ戻す。 */
export function changeCodexSelection(select: HTMLSelectElement, confirmed: string, submit: (value: string) => void): void {
  const requested = select.value
  select.value = confirmed
  submit(requested)
}
