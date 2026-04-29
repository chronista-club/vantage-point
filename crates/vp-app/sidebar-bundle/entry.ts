/**
 * vp-app sidebar 用 minimal entry point.
 *
 * iconify-icon Web Component を customElements.define() で register するだけの薄い bundle。
 * SolidJS を含まず、 sidebar の vanilla HTML 内で `<iconify-icon icon="ph:compass">`
 * のような element を使えるようにする。
 *
 * Build:
 *   cd crates/vp-app/sidebar-bundle && bun install && bun run build
 *
 * 出力: ../assets/sidebar-icons.bundle.js (vp-app の Rust 側で include_bytes!)
 *
 * Phase E1 (skeleton): runtime fetch (api.iconify.design) で icon 取得。
 * Phase E1b (offline 化): @iconify-json/ph 等を bundle して addCollection で preload 予定。
 */

import 'iconify-icon'
