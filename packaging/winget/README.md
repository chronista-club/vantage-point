# winget packaging — Chronista.VantagePoint

Homebrew cask（`chronista-club/homebrew-tap`）の **Windows 側カウンターパート**。
Mac の `.dmg` → cask に対して、Windows は `vp.exe`（portable）→ winget manifest で配る。

現状は **手動 manifest + ローカル検証** の段階（dogfood）。公開 `microsoft/winget-pkgs`
への提出と Authenticode 署名は後続フェーズ。

## 同梱物

- `vp.exe`（CLI + 常駐 daemon TheWorld）のみ。GUI（vp-app）は未同梱。
- `InstallerType: portable` — winget が exe を Links dir に置き PATH を通す。MSI 不要。

## manifest ファイル（multi-file、ManifestVersion 1.6.0）

| file | 役割 |
|---|---|
| `Chronista.VantagePoint.yaml` | version manifest（束ねる root） |
| `Chronista.VantagePoint.installer.yaml` | installer（URL / sha256 / architecture / portable command） |
| `Chronista.VantagePoint.locale.en-US.yaml` | メタデータ（publisher / license / description） |

version を上げるときは **3 ファイルすべての `PackageVersion`** と、installer の
`InstallerUrl` / `InstallerSha256` を更新する。

## ビルド → sha256

```powershell
cargo build --release -p vp-cli
$sha = (Get-FileHash target\release\vp.exe -Algorithm SHA256).Hash
# installer.yaml の InstallerSha256 に $sha を反映（大文字/小文字どちらでも可）
```

## schema 検証（オフライン、URL 到達不要）

```powershell
winget validate --manifest packaging\winget
```

## end-to-end インストール検証（ローカル HTTP でホスト）

公開 release にまだ Windows 資産を添付していないため、`InstallerUrl` の本番 URL は
到達しない。実インストールを試すときは、ビルドした exe を localhost でホストし、
`InstallerUrl` を localhost に差し替えた **一時 manifest** で回す:

```powershell
# 1. exe を temp にコピーして HTTP でホスト（別ターミナル）
$pub = "$env:TEMP\vp-winget-serve"
New-Item -ItemType Directory -Force $pub | Out-Null
Copy-Item target\release\vp.exe "$pub\vp-x86_64-pc-windows-msvc.exe"
cd $pub; python -m http.server 8099   # or any static server

# 2. manifest を temp にコピーし InstallerUrl を localhost に差し替え
#    （sha256 は同一ファイルなので変更不要）
$dst = "$env:TEMP\vp-winget-manifest"
Copy-Item -Recurse -Force packaging\winget $dst
(Get-Content $dst\Chronista.VantagePoint.installer.yaml) `
  -replace 'InstallerUrl:.*', 'InstallerUrl: http://localhost:8099/vp-x86_64-pc-windows-msvc.exe' `
  | Set-Content $dst\Chronista.VantagePoint.installer.yaml

# 3. インストール（初回のみ管理者で `winget settings --enable LocalManifestFiles` が必要）
winget install --manifest $dst
vp --version
# uninstall: local-manifest 由来は ARP id が `..__DefaultSource` になり ID 一致しないため
# name 指定で外す（公開 winget source からの install なら `winget uninstall --id ...` が効く）
winget uninstall --name "Vantage Point"
```

> ⚠️ `winget install --manifest` は初回に一度だけ、管理者権限で
> `winget settings --enable LocalManifestFiles` を有効化する必要がある（`--disable` で戻せる）。
> 公開 `winget-pkgs` に載せた後の通常の `winget install VantagePoint` ではこのトグルは不要。

## 公開フェーズ（後続）

1. GitHub Release に `vp-x86_64-pc-windows-msvc.exe` を添付（`release:win` タスクで自動化予定）。
2. Authenticode 署名（`signtool`）を release パイプラインに組み込み、SmartScreen 警告を解消。
3. `wingetcreate` で `microsoft/winget-pkgs` に PR（cask の `release:cask` に相当する
   `release:winget` タスクで自動 update）。
