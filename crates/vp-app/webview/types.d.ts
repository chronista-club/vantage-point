// pp-content-persist follow-up: esbuild の text loader (build.mjs) で CSS を文字列として
// import するための ambient declaration。 LSP に「`.css` import は string になる」 と伝える。
declare module '*.css' {
  const css: string
  export default css
}
