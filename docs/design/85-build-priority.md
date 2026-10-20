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

macOS の LaunchAgent は `ProcessType=Interactive` とする。従来の `Background` では
daemon と子孫が background に制約され、`Standard` にした後も daemon の CPU 時間は
約 99% utility だった（2026-10-21、lead 実測）。daemon は入力のエコー、wire 中継、
lane 表示を担う開発エディタの半身なので、build と同じ制約下に置かない。
GUI との通信は QUIC であり、XPC の activity に依存する `Adaptive` は使えない。
`Interactive` は launchd の資源制限の分類で、全 thread を user_interactive に固定する指定ではない。

### QoS の段組み

裁定: `mem_1CgDj663HTMRwEu1BN3N4C`、実装 task: `mem_1CgDj7Ks1Znq41SQqTkUg1`。

| 層 | 指定 / 期待 | 入れ方 |
|---|---|---|
| GUI（vp-app） | user_interactive | 既存のアプリ起動経路 |
| daemon | Interactive（launchd ProcessType） | plist |
| lane の shell / claude / codex（PTY・chat） | default を期待、反映後に実測 | 通常 spawn、XNU の継承に任せる |
| build（mise 経由） | utility | 既存の `VP.build_run` |

[XNU の `task_compute_main_thread_qos`](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/kern/task_policy.c#L2212-L2251)
は `DAEMON_INTERACTIVE` の main thread を `THREAD_QOS_LEGACY` にする。
[`task_policy_create`](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/osfmk/kern/task_policy.c#L734-L746)
では子が親の app type と clamp を継承する。このコードから、通常 spawn の子は
default で始まると期待する。`QOS_CLASS_DEFAULT` は
[libpthread の対応表](https://github.com/apple-oss-distributions/libpthread/blob/42d026df5b07825070f60134b980a1ec2552dfee/src/qos.c#L69-L78)
では `THREAD_QOS_LEGACY` であり、下記 probe の `legacy` 欄に対応する。
これは公開ソースからの推論で、使用中の macOS での実効値や、子が後から thread の QoS を
変更しないことを保証しない。default の強制上限は設けない。

2026-10-21、utility 制約下の lane で C の小実験を行い、次を確認した:

- `posix_spawnattr_set_qos_class_np(DEFAULT)` は `EINVAL(22)`、`UTILITY` は成功。
  [Apple の実装](https://github.com/apple-oss-distributions/libpthread/blob/42d026df5b07825070f60134b980a1ec2552dfee/src/qos.c#L633-L644)
  も utility / background / maintenance だけを受理する。`taskpolicy -c default` も拒否された。
- 親 thread を `pthread_set_qos_class_self_np(DEFAULT, 0)` で変更すると呼び出しは成功し、
  fork 直後は default だったが、exec 後・通常 posix_spawn の子は utility に戻った。
- fork 後の子で `pthread_set_qos_class_self_np(DEFAULT, 0)` を呼ぶと `EPERM(1)`。
  exec 前の一時的な thread 指定を子プロセスの永続 QoS 設定として扱えない。

この結果を受け、PTY / chat engine の spawn に QoS wrapper を追加しない。
private API や専用 launcher も導入しない。Interactive 反映後の実効値と watchdog の
誤検知解消は未確認であり、lead の実機検証に残す。

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
Interactive 反映後の効果測定は別途必要。

## 既存インストールへの反映

`commands/daemon.rs::install` → `process.rs::install_launch_agent` は既存 plist を
`std::fs::write` で上書きし、`launchctl bootout` → `bootstrap` で読み直す。
bootout 後は `launchctl print` で登録解除を 100ms 間隔・上限 30 秒で確認し、
2 秒経過時に停止待ちの案内を stderr に一度だけ出す。
bootstrap を最大 3 回（間隔 500ms）試す。最後に指定 port の health を最大 30 秒待ち、
応答がない場合も失敗として復旧コマンドを返す。install の成功表示は health 確認後に限る。
`repair_launch_agent` も同じ関数を通る。uninstall は再登録を行わないため、この待機を使わない。

テストでは launchctl・health・時計を差し替える。実機の install は daemon 配下の lane を
停止させるため、VP の外の terminal で確認する（`mem_1CgDiXr51YQzyg6Hoo4wp7`）。

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
既存の plist 生成テストで `ProcessType=Interactive` を固定する。
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
Interactive 反映後は lead が daemon、新しい lane の shell / claude / codex（PTY と chat）、
`mise run check` の rustc を測る。lane は default（probe の `legacy`）、
rustc は utility の CPU 時間が増えることを確認する。
lane の子に user_initiated / user_interactive が観測された場合は別 task で対応を判断する。
daemon の QoS 分布、同じ build の所要時間、1 週間の watchdog self-heal 件数も記録し、
plist の単体テスト通過と実機での効果を区別する。

### Interactive 反映後の実測（2026-10-21）

`vp daemon install`（VP の外の terminal）と GUI の開き直し後、
`proc_pid_rusage(RUSAGE_INFO_V4)` の CPU 時間の内訳は次のとおり。

| 層 | 2026-10-20（Background） | 2026-10-21（Interactive） |
|---|---|---|
| GUI（vp-app） | — | user_interactive 90% |
| daemon | background 100% | default（legacy）99% |
| lane の shell | background 100% | default（legacy）100% |
| codex app-server | background | default（legacy）98% |
| `mise run check` の mbx / cargo / rustc | background | utility 99〜100% |

daemon は Interactive でも main thread は legacy（default）で、lane の agent と同じ段になる。
build は helper で utility に下がるので、daemon は build より上にいる。

同じ手順（source `1d2f8578`、依存を温めた seed を APFS clone、
`cargo build --locked -p vantage-point` で VP 本体だけを 2 本並走、wrapper なし）で
所要時間を比べた。各条件 1 回の測定。

| 条件 | 再コンパイル（2 本並走） | health p50 / p95 / max（ms） | load1 max |
|---|---:|---:|---:|
| 2026-10-20（lane の実効 QoS は background） | 250〜278 秒 | 4.3〜4.7 / 19〜31 / 367〜566 | 10.5〜16.2 |
| 2026-10-21 default | 41 秒 | 0.6 / 2.9 / 142 | 8.8 |
| 2026-10-21 utility（helper と同じ指定） | 55 秒 | 0.6 / 1.0 / 8.9 | 10.7 |

依存 691 crate の seed build も 2026-10-21 は約 2 分だった。日も機材の状態も違うので小差は
誤差として扱うが、5〜7 倍の差は background の制約（効率コアへの寄せと IO の絞り）で説明できる。

utility は default より build が約 3 割遅い代わりに、daemon の health の最大値が小さい。
エディタと daemon の応答を優先し（mako 2026-10-21）、build の utility 指定は残す。
急ぐ場合は `VP_BUILD_PRIORITY=normal mise run …` で 1 回だけ default で走らせるか、VP の外の terminal で mise を通さず cargo を直接打つ（外の terminal でも `mise run` は helper が効いて utility になる）。

## Status log

- 2026-10-20: 測定後の方式選択を受け、開発 task 用の共通 helper を追加。
- 2026-10-20: 親の background 制約を実測し、mako GO で LaunchAgent の Standard 化を同梱。
- 2026-10-21: Standard の daemon が utility となる実測を受け Interactive に変更。
  小実験と lead 裁定により、lane の default は XNU の通常 spawn に任せ、強制 wrapper は追加しない。
- 2026-10-21: install の bootout 直後の競合に対し、登録解除待ち・bootstrap 再試行・health 確認を追加。
- 2026-10-21: lead の停止時間の観測を受け、解除・health 待ちを各 30 秒にし、停止待ちの案内を追加。
- 2026-10-21: Interactive 反映後の QoS と build 所要時間を実測。build の utility 指定を残す裁定を記録。
