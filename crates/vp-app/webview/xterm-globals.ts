/**
 * xterm.js + addon を npm 依存として bundle に取り込み、World A（`main_area.rs` の inline JS）が
 * 期待する **global の形のまま** window に生やす。
 *
 * ## なぜ window 経由か（一時的な形）
 *
 * xterm を操る 976 行はまだ `main_area.rs` の inline `<script>`（= World A）にある。この PR は
 * **依存の供給路だけ**を vendored `<script>` から npm bundle へ移す段で、inline JS には 1 行も
 * 触らない（壊れ方を「xterm が来なければ console が即黒」の一点に絞るため）。
 * inline JS を TS module へ移す次段でこの file は消え、`createLaneInstance` が直 import する。
 *
 * ## 形を UMD に合わせる理由
 *
 * 旧 vendored asset は UMD で、addon は `globalThis.FitAddon = { FitAddon: class }` という
 * **入れ子**を作っていた（inline JS は `new FitAddon.FitAddon()` と書いている）。一方 xterm 本体は
 * `for (s in exports) globalThis[s] = exports[s]` の展開なので `globalThis.Terminal` は**クラス直**。
 * この非対称は upstream の UMD 定義の違いで、inline JS を無改変に保つには**そのまま再現する**のが正。
 *
 * ## 実行順序
 *
 * この module は `entry.tsx` 経由で `editor-host.bundle.js` に入る。bundle は
 * `<script src>`（`defer`/`async` 無し = classic blocking）で読まれ、**inline `<script>` より
 * 文書順で先**に実行されるので、inline JS が `new Terminal(...)` する時点で global は必ず揃う。
 * （旧 vendored `<script>` も同じ位置＝ inline の直前だったので、順序は不変。）
 *
 * ## ImageAddon を積まない
 *
 * 旧構成は `addon-image.js`（79KB）を `<script>` で読んでいたが、実体の参照は**コメント 2 行だけ**
 * （main_area.rs の「❌ ImageAddon — VP-162 で不要判定 (2026-05-11)」）。不要と判定済みのものを
 * 積み続けていたので、この移管で落とす。必要になったら `@xterm/addon-image` を足せばよい。
 */
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { ProgressAddon } from "@xterm/addon-progress";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { UnicodeGraphemesAddon } from "@xterm/addon-unicode-graphemes";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";

const w = window as unknown as Record<string, unknown>;

// xterm 本体: UMD が exports を globalThis に展開していたので、クラスを直に載せる。
w.Terminal = Terminal;

// addon: UMD の namespace object 形（`new XAddon.XAddon()`）を再現する。
w.FitAddon = { FitAddon };
w.ProgressAddon = { ProgressAddon };
w.Unicode11Addon = { Unicode11Addon };
w.UnicodeGraphemesAddon = { UnicodeGraphemesAddon };
w.WebLinksAddon = { WebLinksAddon };
w.WebglAddon = { WebglAddon };
