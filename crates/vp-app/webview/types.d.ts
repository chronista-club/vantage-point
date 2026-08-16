// pp-content-persist follow-up: esbuild の text loader (build.mjs) で CSS を文字列として
// import するための ambient declaration。 LSP に「`.css` import は string になる」 と伝える。
declare module '*.css' {
  const css: string
  export default css
}

// code pane（コードブラウザ P1）の bundle 間 API。
// 実体は main bundle（entry.tsx が expose）、呼び手は sidebar bundle（directive f / p、
// LaneRow フォルダボタン）。webview は 1 document 2 bundle なので window global が橋。
// ⚠️ bundle 評価前の窓では undefined — 呼び手は optional chain で no-op に倒すこと。
interface Window {
  vpCodePane?: {
    /** 開閉 toggle（active lane 不在は main 側で no-op）。 */
    toggle(): void
    /** 指定 lane の pane を開く（LaneRow フォルダ — lane 切替と併用する片方向）。 */
    openFor(address: string): void
    /** 選択中（または開いている）file を board へ投擲。 */
    sendSelectedToBoard(): void
  }
}
