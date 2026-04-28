// Phase 5-C: vp-app webview の Nerd Font loader (PlemolJP Console NF + 35Console NF)。
// web_assets.rs の NERD_FONT_LOADER_JS から include_str! 参照、 SIDEBAR_HTML 等の <script> 冒頭に注入。
//
// WKWebView は CSS @font-face url() を WKURLSchemeHandler に投げない既知制限があるため、
// fetch() で取得した ArrayBuffer から FontFace を作って document.fonts に手動 register する。
// VPMono と VPMono35 の 2 family を 32 variant 全てまとめて Promise.all で並列 fetch、
// 完了後 state を再 apply して既描画 .nf-icon の font resolution を更新する。

(async () => {
  // [family-name, file-prefix] のペア
  const families = [['VPMono', 'plemol'], ['VPMono35', 'plemol35']];
  // [variant-suffix, font-weight, font-style] の 16 entries
  const variants = [
    ['thin',           '100', 'normal'], ['thinitalic',       '100', 'italic'],
    ['extralight',     '200', 'normal'], ['extralightitalic', '200', 'italic'],
    ['light',          '300', 'normal'], ['lightitalic',      '300', 'italic'],
    ['text',           '350', 'normal'], ['textitalic',       '350', 'italic'],
    ['regular',        '400', 'normal'], ['italic',           '400', 'italic'],
    ['medium',         '500', 'normal'], ['mediumitalic',     '500', 'italic'],
    ['semibold',       '600', 'normal'], ['semibolditalic',   '600', 'italic'],
    ['bold',           '700', 'normal'], ['bolditalic',       '700', 'italic'],
  ];
  const tasks = [];
  for (const [family, prefix] of families) {
    for (const [name, weight, style] of variants) {
      tasks.push((async () => {
        try {
          const r = await fetch('vp-asset://font/' + prefix + '-' + name + '.ttf');
          if (!r.ok) throw new Error('HTTP ' + r.status);
          const buf = await r.arrayBuffer();
          const f = new FontFace(family, buf, { weight, style, display: 'block' });
          await f.load();
          document.fonts.add(f);
        } catch (e) {
          console.warn('[vp-asset] font load failed:', family, name, e);
        }
      })());
    }
  }
  await Promise.all(tasks);
  // WebKit は font 登録後の既存要素の font resolution を自動更新しないため、
  // 既存 .nf-icon (state push で作られた) を再生成して font を拾わせる。
  if (typeof state !== 'undefined' && state && typeof applyState === 'function') {
    applyState(state);
  }
})();
