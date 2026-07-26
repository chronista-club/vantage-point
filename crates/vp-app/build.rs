//! Windows 向け resource 埋め込み — exe icon + VersionInfo。
//!
//! Windows は「exe に焼かれた icon resource」を Explorer / taskbar / Alt-Tab /
//! ショートカットの見た目に使う。 これが無いと generic な白アイコンになる
//! (mac の `.app` に icon.icns を同梱するのと同じ役割)。
//!
//! icon.ico は `assets/icon.png` (1024x1024、 portal favicon 由来の SSOT) から
//! 生成した 7 解像度 (16/24/32/48/64/128/256) の multi-resolution icon。
//! icon.png を差し替えたら icon.ico も再生成すること。
//!
//! **mac / Linux では丸ごと no-op**。 target が windows の時だけ resource を compile する
//! (Windows 対応で mac build を退行させない)。

fn main() {
    // webview bundle の存在ガード + 変更検知（全 OS 共通、2026-07-19）。
    //
    // assets/*.bundle.js は gitignore の生成物（bundle commit 運用は廃止 — 依存を npm 化した
    // ことで「commit で pin する」必要が消えた）。include_str!（main_area.rs）が不在時に出す
    // 不可解なコンパイルエラーの前に、生成手順を案内して fail する。
    // rerun-if-changed は「bundle を作り直したのに cargo が embed を取りこぼす」旧 footgun
    // （touch main_area.rs の儀式）の根治。
    // xterm.css も同じ生成物（build.mjs が node_modules/@xterm/xterm から複写、xterm 本体と
    // addon は bundle に取り込み済み）。vendored copy を手で維持すると `bun update` で
    // JS と CSS が黙って版ズレするため、供給路を npm 1 本に寄せてある。
    for bundle in [
        "assets/editor-host.bundle.js",
        "assets/sidebar.bundle.js",
        "assets/xterm.css",
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(bundle);
        assert!(
            path.exists(),
            "webview bundle が未生成です: {bundle}\n\
             先に `mise run app:bundle`（または crates/vp-app/webview で \
             `bun install --frozen-lockfile && bun run build`）を実行してください。"
        );
        println!("cargo:rerun-if-changed={bundle}");
    }

    // target 判定は cfg block の **中** で行う。 外に出すと非 windows では後続が cfg で消えて
    // `return;` が関数末尾になり、 clippy::needless_return が出る (Windows host の clippy では
    // 原理的に踏めず、 mac CI でだけ落ちる)。
    #[cfg(windows)]
    {
        // build.rs 内の `cfg!(windows)` は **host** を指してしまうので、 target を見る。
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if target_os != "windows" {
            return;
        }

        println!("cargo:rerun-if-changed=assets/icon.ico");

        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        // VersionInfo — タスクマネージャの「説明」列や exe のプロパティに出る。
        // FileVersion / ProductVersion は CARGO_PKG_VERSION から winresource が自動で入れる。
        res.set("ProductName", "Vantage Point");
        res.set(
            "FileDescription",
            "Vantage Point — AI native development environment",
        );
        res.set("CompanyName", "Chronista Club");
        res.set("OriginalFilename", "vp-app.exe");

        // 埋め込み失敗は build を落とす (silent にアイコンが欠けると気付けないため)。
        res.compile()
            .expect("Windows resource (icon + VersionInfo) の埋め込みに失敗");
    }
}
