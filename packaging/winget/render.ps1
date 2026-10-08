# winget manifest を生成する — Chronista.VantagePoint
#
# Homebrew の `mise run release:cask`（cask の version / sha256 を書き換える）に当たる。
# Release に添付する zip（vp.exe + vp-app.exe）の sha256 から multi-file manifest 3 枚を書く。
# CI（.github/workflows/release-windows.yml）と手元の両方からこの 1 本を呼ぶ。
#
#   pwsh packaging/winget/render.ps1 -Version 0.82.0 -ZipSha256 <hex> -OutDir dist/winget
#
# -InstallerUrl を渡すと URL だけ差し替える（localhost でホストしたローカル検証用）。
param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $ZipSha256,
    [Parameter(Mandatory)] [string] $OutDir,
    [string] $InstallerUrl = ''
)
$ErrorActionPreference = 'Stop'

$Version = $Version.TrimStart('v')
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "version が不正: $Version" }
if ($ZipSha256 -notmatch '^[0-9A-Fa-f]{64}$') { throw "sha256 が不正: $ZipSha256" }
if (-not $InstallerUrl) {
    $InstallerUrl = "https://github.com/chronista-club/vantage-point/releases/download/v$Version/VantagePoint-$Version-x86_64-pc-windows-msvc.zip"
}

$id = 'Chronista.VantagePoint'
$manifestVersion = '1.6.0'
# .NET の WriteAllText は PowerShell の現在位置でなくプロセスの cwd で相対 path を解くので絶対 path にする
$OutDir = (New-Item -ItemType Directory -Force $OutDir).FullName

# ⚠️ 下の here-string は展開あり（"@...@"）。本文に backtick を書くと escape になる
#（`v が垂直タブになり winget validate が control characters で落ちた）。
$files = @{
    "$id.yaml" = @"
PackageIdentifier: $id
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $manifestVersion
"@
    # zip の中の exe 2 つを portable として置き、両方に command alias を張る。
    # vp-app も PATH に載るので `vp app start` が PATH 経由で GUI を見つける。
    "$id.installer.yaml" = @"
PackageIdentifier: $id
PackageVersion: $Version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: vp.exe
    PortableCommandAlias: vp
  - RelativeFilePath: vp-app.exe
    PortableCommandAlias: vp-app
Commands:
  - vp
  - vp-app
Installers:
  - Architecture: x64
    InstallerUrl: $InstallerUrl
    InstallerSha256: $($ZipSha256.ToUpperInvariant())
ManifestType: installer
ManifestVersion: $manifestVersion
"@
    "$id.locale.en-US.yaml" = @"
PackageIdentifier: $id
PackageVersion: $Version
PackageLocale: en-US
Publisher: Chronista Club
PublisherUrl: https://github.com/chronista-club
PublisherSupportUrl: https://github.com/chronista-club/vantage-point/issues
PackageName: Vantage Point
PackageUrl: https://github.com/chronista-club/vantage-point
License: MIT OR Apache-2.0
LicenseUrl: https://github.com/chronista-club/vantage-point/blob/main/LICENSE
Copyright: Copyright (c) Chronista Club
ShortDescription: AI-native development environment — vp CLI, daemon and GUI.
Description: |-
  Vantage Point (vp) is a Rust-based AI-native development environment that uses
  the Claude CLI as its engine, integrating a TUI console, Canvas (WebView), and
  external control. This package installs the vp command-line tool (with its
  resident daemon) and the vp-app GUI for Windows.
Moniker: vantage-point
Tags:
  - ai
  - cli
  - claude
  - developer-tools
  - rust
ManifestType: defaultLocale
ManifestVersion: $manifestVersion
"@
}

foreach ($name in $files.Keys) {
    # winget は BOM なし UTF-8 / LF を素直に読む
    $path = Join-Path $OutDir $name
    [System.IO.File]::WriteAllText($path, ($files[$name] -replace "`r`n", "`n") + "`n")
    Write-Host "wrote $path"
}
