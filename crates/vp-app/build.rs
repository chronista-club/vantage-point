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
