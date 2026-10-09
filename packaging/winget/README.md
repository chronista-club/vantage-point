# winget packaging — Chronista.VantagePoint

Homebrew cask（`chronista-club/homebrew-tap`）の **Windows 側カウンターパート**。
Mac の `.dmg` → cask に対して、Windows は zip（`vp.exe` + `vp-app.exe`）→ winget manifest で配る。

現状は **Release に manifest を添付し、手元で `winget install --manifest`** する段階（dogfood）。
公開 `microsoft/winget-pkgs` への提出と Authenticode 署名は後続フェーズ。

## 同梱物

- `vp.exe`（CLI + 常駐 daemon）と `vp-app.exe`（GUI）を 1 つの zip に入れる。
- `InstallerType: zip` + `NestedInstallerType: portable` — winget が zip を展開し、両 exe の
  symlink を Links dir に置いて PATH を通す（`vp` / `vp-app` が即使える）。MSI 不要。
  `vp app start` は PATH 上の `vp-app` を見つける（symlink を解決した実体の隣も探す）。
- **Start Menu shortcut は winget では作られない**（portable の制約）ので、**vp-app が起動のたびに
  自分で用意する**（`vp app install --target <自分> --if-changed` を裏で呼ぶ。AUMID 付きなので
  ピン留めも効く）。初回だけ `vp app start` で GUI を開けば、以後は Win キー →「Vantage Point」。
  消すのは `vp app uninstall`。dev profile / cargo の build 出力から起動した時は触らない。

## 生成の流れ（自動）

Mac で Release を publish すると `.github/workflows/release-windows.yml` が Windows runner で
同じ tag を build し、次を Release に添付する:

| asset | 用途 |
|---|---|
| `vp-x86_64-pc-windows-msvc.exe` / `vp-app-x86_64-pc-windows-msvc.exe` | `vp update` の Windows 経路 |
| `VantagePoint-<ver>-x86_64-pc-windows-msvc.zip` | winget の installer |
| `Chronista.VantagePoint*.yaml`（3 枚） | winget manifest（`render.ps1` が zip の sha256 から生成） |
| `SHA256SUMS-x86_64-pc-windows-msvc.txt` | exe / zip の sha256 |

manifest は生成物なので repo には置かない（version ごとの手書き manifest は廃止）。
`render.ps1` は cask の `mise run release:cask` に当たる。workflow は `render.ps1` を
**workflow 側の ref** から取るので、`render.ps1` を持たない古い tag にも workflow_dispatch で後付けできる。

## 手元でインストール

```powershell
$dir = "$env:TEMP\vp-winget\0.82.0"
gh release download v0.82.0 -R chronista-club/vantage-point -p "Chronista.VantagePoint*.yaml" -D $dir
winget install --manifest $dir
vp --version
```

> ⚠️ `winget install --manifest` は初回に一度だけ、管理者権限で
> `winget settings --enable LocalManifestFiles` を有効化する必要がある（`--disable` で戻せる）。
>
> ⚠️ local manifest 由来は ARP id が `Chronista.VantagePoint__DefaultSource` になり `--id` で
> 外せない。uninstall は `winget uninstall --name "Vantage Point"`。

## manifest だけ作る / 検証する

```powershell
# zip の sha256 から 3 枚を書く。-InstallerUrl で localhost 等に差し替えられる（ローカル検証用）
pwsh packaging\winget\render.ps1 -Version 0.82.0 -ZipSha256 <hex> -OutDir $env:TEMP\vp-manifest
winget validate --manifest $env:TEMP\vp-manifest
```

Release 前の zip を試すときは、zip を localhost でホストし `-InstallerUrl` をそこに向けた
manifest で `winget install --manifest` する（hash 検証は同じ zip なので通る）。

## 公開フェーズ（後続）

1. ~~GitHub Release に Windows 資産を添付~~ → `release-windows.yml` で自動化済み。
2. ~~manifest の生成~~ → `render.ps1` + `release-windows.yml` で自動化済み。
3. Authenticode 署名（`signtool`）を release パイプラインに組み込み、SmartScreen 警告を解消。
4. `wingetcreate` / `komac` で `microsoft/winget-pkgs` に PR（公開 source から `winget install` できるようにする）。
5. winget で入れた環境での `vp update`（exe を直接差し替える）と winget の管理の折り合い。
