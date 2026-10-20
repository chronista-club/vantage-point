# 85. Build の実行優先度

> **Status**: Draft
> **Related**: `mem_1CgDHarsYzSdNQbvLzYqoQ`（起票・方式選択）
> **対象**: `scripts/mise/vp.rb`, `.mise/tasks/`, `daemon/process.rs`

## 目的と構成

重い build が daemon / GUI と CPU を取り合う場面に備え、開発用 task の
外部コマンドを `VP.build_run(*argv, **opts)` に集約する。CPU 使用率や jobs の
上限を設定するものではなく、実行優先度を下げる。watchdog の判定は変更しない。

| 環境 | 実行方法 |
|---|---|
| macOS、PATH に実行可能な taskpolicy がある | `taskpolicy -c utility <command> ...` |
| 他の Unix、または macOS で taskpolicy が無い | `nice -n 10 <command> ...` |
| Windows、または使える優先度コマンドが無い | 通常実行 |
| `VP_BUILD_PRIORITY=normal` | OS にかかわらず通常実行 |

`build_prefix` が OS・環境変数・PATH から引数列を選び、`build_run` は既存の
`VP.run` に渡す。実行可能な通常ファイルだけを採用し、絶対パスで呼ぶ。
引数は shell 文字列に連結せず、`chdir` などの spawn option と失敗時の終了コードを
維持する。既存の `VP.sh` / `VP.run` の呼び出し全体には適用しない。

対象は `check` / `test` の mbx、`release:mac` の cargo build、
`app:build` / `daemon:build` の cargo install、`app:bundle` の bun install / build、
`dev:gc` の cargo sweep。GC の削除判定や build 中の guard は変更しない。
branch-step の `app:bundle && check` も同じ入口を通る。

macOS の LaunchAgent は `ProcessType=Standard` を前提とする。従来の `Background` では
daemon とその子孫（lane の shell、claude / codex、cargo）が background に制約され、
build に utility を指定しても優先度の差を作れなかった。daemon 側の制約を外した上で
build の子プロセスだけを utility に下げる。これは watchdog の応答遅延リスクを下げるための
変更であり、誤検知の解消を実機で確認したものではない。

一時的に通常優先度で比較したい場合:

```bash
VP_BUILD_PRIORITY=normal mise run check
```

lane で直接 build するときの規約は `AGENTS.md` を参照。

## 方式選択の材料

2026-10-20、macOS 27.0.1 / P4 + E6 / 24 GB / Cargo 1.96.1 /
source `1d2f8578` で測定した。`cargo build --locked -p vantage-point` を
独立した target に 2 本ずつ並走させた。jobs は既定。
依存を準備した同一 seed から各条件の前に APFS clone し、VP 本体だけ再コンパイルした。

| 条件 | 各 build / 平均（秒） | health p50 / p95 / max（ms） | 1 秒超 | load1 max |
|---|---:|---:|---:|---:|
| 通常 | 278.09 / 278.09 / 278.09 | 4.6705 / 31.209 / 565.644 | 0 | 14.318 |
| nice -n 10 | 266.07 / 266.07 / 266.07 | 4.4275 / 25.464 / 478.396 | 0 | 16.152 |
| taskpolicy -c utility | 250.10 / 250.10 / 250.10 | 4.3245 / 18.640 / 366.855 | 0 | 10.520 |
| taskpolicy -b | 251.10 / 251.10 / 251.10 | 4.513 / 25.633 / 475.653 | 0 | 10.790 |

build の終了検出は約 1 秒粒度。health は `/api/health` の curl `time_total` を
約 1 秒おきに採取し、p50 は中央値、p95 は nearest rank。全 8 build が成功、
health 計 1,045 点はすべて HTTP 200。停止条件は HTTP 失敗または 5 秒超が 2 回連続。

この測定では utility 指定時の所要時間と p95 / max が最小だった。
ただし固定順で各条件 1 回の測定差は誤差範囲として扱い、「速くなる」とは主張しない。
OS cache・温度・通常アプリ負荷の順序効果は分離しておらず、load 30 の障害条件は未再現。
full cold build / bun build の比較、P/E コア割当の直接計測、
QUIC watchdog の誤検知解消はこの測定では検証していない。

さらに後続の `mise run check` では、utility を指定した rustc の
`proc_pid_rusage(RUSAGE_INFO_V4)` が utility=0、background の CPU 時間のみ増加した。
同じ lane 内の短い CPU probe も通常・utility 指定とも background のみだった。
lead 側では daemon と lane shell も同様だった。元の比較中には親の QoS を採取していないため、
上表を「実効 QoS が異なる 4 条件の比較」とは扱えない。utility は方式として採用し、
Standard 反映後の効果測定は別途必要。

## 既存インストールへの反映

`commands/daemon.rs::install` → `process.rs::install_launch_agent` は既存 plist を
`std::fs::write` で上書きし、`launchctl bootout` → `bootstrap` で読み直す。
bootstrap が非ゼロなら警告を出し、配置済み plist を次回 login に読み込む設計なので、
install の終了コードだけで即時反映を判定しない。

[homebrew-tap の Cask](https://github.com/chronista-club/homebrew-tap/blob/e3cfe08c0088df92846c047e777c3378eb531082/Casks/vantage-point.rb)
（2026-10-20 確認、v0.86.0）の postflight は `daemon restart --if-running` を呼び、
install は呼ばない。通常の restart は既存 plist を使う。
`repair_launch_agent` が焼き直すのは kickstart 後の health 確認に失敗した場合なので、
brew upgrade だけではこの設定の移行は保証されない。

新版の release binary を配置後、lead / 利用者が作業を退避した上で一度実行する:

```bash
VP_PROFILE= /Applications/VantagePoint.app/Contents/MacOS/vp daemon install
```

app を別の場所に配置している場合はその実体のパスを使う。install は daemon の再起動を伴う。
既存セッションの QoS が変わるとはみなさず、新しく開始した lane セッションで下記を測る。
lane の検証では install / restart / swap を実行していない。

## 検証と保守

`ruby scripts/mise/tests/build_priority_test.rb`（`mise run test` からも実行）で
OS ごとの選択、コマンド不在、normal 指定、引数・作業ディレクトリ・失敗伝播を確認する。
既存の plist 生成テストで `ProcessType=Standard` を固定する。
Linux / Windows の分岐の単体検証と、各 OS 上の実際の build 成功は区別する。

実機の `taskinfo <pid>` はこの環境では root が必要で利用できなかった。
代わりに `ps -axo pid,ppid,comm` で対象の daemon / shell / rustc を特定し、
次の read-only probe で QoS ごとの累積 CPU 時間を 2 回読む。差分が実際に使った QoS の証拠となる。
nice 値や起動引数だけでは utility の実効性は判定できない。短命な rustc は採取前に終了しうる。

```bash
python3 - 12345 <<'PY'  # 12345 を確認対象の PID に置き換える
import ctypes, json, re, subprocess, sys, time
from pathlib import Path
sdk = subprocess.check_output(["xcrun", "--show-sdk-path"], text=True).strip()
header = (Path(sdk) / "usr/include/sys/resource.h").read_text()
body = re.search(r"struct rusage_info_v4 \{(.*?)\};", header, re.S).group(1)
fields = []
for kind, name, count in re.findall(r"(uint8_t|uint64_t)\s+(\w+)(?:\[(\d+)\])?;", body):
    ctype = ctypes.c_uint8 if kind == "uint8_t" else ctypes.c_uint64
    fields.append((name, ctype * int(count) if count else ctype))
class Usage(ctypes.Structure):
    _fields_ = fields
lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
lib.proc_pid_rusage.restype = ctypes.c_int
for sample in range(2):
    usage = Usage()
    if lib.proc_pid_rusage(int(sys.argv[1]), 4, ctypes.byref(usage)) != 0:
        raise OSError(ctypes.get_errno(), "proc_pid_rusage failed")
    print(json.dumps({"sample": sample, "pid": int(sys.argv[1]), **{
        name: getattr(usage, name) for name, _ in fields if "cpu_time_qos" in name}}))
    if sample == 0:
        time.sleep(0.2)
PY
```

反映前の実測例（rustc PID 83449、同一プロセスを 2 回採取）:
background `76605718 → 95895485`、utility `0 → 0`。通常 probe PID 95302 は
background `6023895` / utility `0`、utility 指定 probe PID 95308 は
background `9249388` / utility `0` だった。
原ログ・probe・比較測定は測定機の `/tmp/vp-build-priority.cumFlG/` に保存
（一時領域なので永続保管先ではない）。
Standard 反映後は daemon / shell が background のみに固定されていないこと、
新しい `mise run check` の rustc で utility の CPU 時間が増えることを確認する。

## Status log

- 2026-10-20: 測定後の方式選択を受け、開発 task 用の共通 helper を追加。
- 2026-10-20: 親の background 制約を実測し、mako GO で LaunchAgent の Standard 化を同梱。
