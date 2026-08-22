//! code pane（コードブラウザ）の file 供給 — lane workdir walk + ファイル読み
//!
//! ## 役割
//!
//! `code:list` / `code:read` IPC（`schema/vp-push.kdl` の応答 event と対、
//! 要求は main webview の CodePane.tsx 発）の Rust 側実装。
//! - `list_entries`: lane workdir 配下のファイルツリーを `.gitignore` 尊重で列挙
//! - `read_file`: pane 内表示用の raw text（`{"text"} | {"error"}` の 2 択）
//!
//! （旧 Sidebar File Explorer overlay picker の Rust 側が前身。picker は code pane 化で
//! 退役し、board への投擲もオミットされた — 旧投擲経路（"show" の WebView 直注入）は
//! board 化 #771 で受け手が消えて既に死んでいた。walk の実装はそのまま供給源として残る。）
//!
//! ## 設計判断
//!
//! - **walk は同期実行を caller が thread に逃す**: I/O blocking のため
//!   wry/tao の main thread からは呼ばず、 caller (event loop) で
//!   `thread::Builder::spawn` してから呼ぶ。 結果は `AppEvent` で push back。
//! - **path traversal 防御**: `read_file` は `rel_path` を `Component::Normal` のみで
//!   構成されていることを確認し、 `..` / 絶対パス / hidden を弾く。
//! - **巨大ファイル**: text 1 MiB 超は `{"error"}` に降格。 walk は 20,000 件で
//!   truncate して `truncated: true` flag。

use ignore::WalkBuilder;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

/// walk の最大エントリ数。 超過したら truncate + flag。
pub const DEFAULT_LIST_LIMIT: usize = 20_000;
/// text / code として開ける単一ファイルの最大サイズ。
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
/// hidden / .gitignore 経由で除外されない、 ノイズの大きい dir を hardcoded で潰す。
const BLOCKED_DIRS: &[&str] = &["target", "node_modules", "dist", "build", ".vp"];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    File,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub rel_path: String,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// lane workdir 配下を walk して entries を返す (default limit)。
pub fn list_entries(workdir: &Path) -> (Vec<Entry>, bool) {
    list_entries_with_limit(workdir, DEFAULT_LIST_LIMIT)
}

/// 任意 limit 版。 unit test 用に小さい limit を渡せるよう分離。
pub fn list_entries_with_limit(workdir: &Path, limit: usize) -> (Vec<Entry>, bool) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut truncated = false;

    let walker = WalkBuilder::new(workdir)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false) // workdir に閉じる: ~/.config/git/ignore は無視
        .parents(false)
        .build();

    for dent in walker.flatten() {
        let path = dent.path();
        let rel = match path.strip_prefix(workdir) {
            Ok(p) if p.as_os_str().is_empty() => continue, // workdir 自身
            Ok(p) => p,
            Err(_) => continue,
        };

        // BLOCKED_DIRS に該当する component が path 上に 1 つでもあれば除外
        if rel.components().any(|c| match c {
            Component::Normal(s) => s
                .to_str()
                .map(|n| BLOCKED_DIRS.contains(&n))
                .unwrap_or(false),
            _ => false,
        }) {
            continue;
        }

        // forward slash 正規化 (TS 側でそのまま rel_path として使う)
        let rel_path = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");

        let kind = match dent.file_type() {
            Some(ft) if ft.is_dir() => EntryKind::Dir,
            Some(ft) if ft.is_file() => EntryKind::File,
            _ => continue, // symlink / device 等は skip
        };

        let size = if matches!(kind, EntryKind::File) {
            dent.metadata().ok().map(|m| m.len())
        } else {
            None
        };

        entries.push(Entry {
            rel_path,
            kind,
            size,
        });

        if entries.len() >= limit {
            truncated = true;
            break;
        }
    }

    // alphabetical: tree 構築 (TS 側) で parent grouping しやすい順
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    (entries, truncated)
}

/// code pane（コードブラウザ P1）の内容表示用にファイルを読む。
///
/// **raw source** を返す。markdown も render しない — pane はコードブラウザなので、
/// `.md` は「描画結果」でなく「ソース」を見せるのが役割。表示側（CodePane.tsx）は
/// Solid の text 挿入（自動 escape）+ `<pre>` で描く。
///
/// 返り値は `{"text": string} | {"error": string}` の 2 択。1 object に両方 optional で
/// 載せると「どちらでもない」が型に載る（vp-push.kdl の ink:snapshot 2 分割と同じ判断だが、
/// ここは受け手が同一 view で本文/理由を出し分けるだけなので 1 event 2 択で閉じる）。
///
/// 画像は error に倒す（text 面では表示できない。board への投擲は用途が見えるまで
/// オミット — mako 2026-08-16）。
pub fn read_file(workdir: &Path, rel_path: &str) -> serde_json::Value {
    let safe_rel = match safe_rel_path(rel_path) {
        Some(p) => p,
        None => return json_error("invalid path (traversal or absolute)"),
    };
    let full = workdir.join(&safe_rel);

    let meta = match std::fs::metadata(&full) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return json_error("not a regular file"),
        Err(e) => return json_error(&format!("metadata error: {e}")),
    };

    match classify_extension(rel_path) {
        FileKind::Markdown | FileKind::Text => {
            if meta.len() > MAX_TEXT_BYTES {
                return json_error(&format!(
                    "file too large ({} bytes, max {MAX_TEXT_BYTES})",
                    meta.len()
                ));
            }
            let bytes = match std::fs::read(&full) {
                Ok(b) => b,
                Err(e) => return json_error(&format!("read failed: {e}")),
            };
            if has_nul_in_prefix(&bytes) {
                return json_error("binary content (NUL byte detected)");
            }
            serde_json::json!({ "text": String::from_utf8_lossy(&bytes).into_owned() })
        }
        FileKind::Image => json_error("image（テキスト表示不可）"),
        FileKind::Unsupported(reason) => json_error(&reason),
    }
}

// =============================================================================
// internals
// =============================================================================

fn json_error(reason: &str) -> serde_json::Value {
    serde_json::json!({ "error": reason })
}

enum FileKind {
    Markdown,
    Text,
    Image,
    Unsupported(String),
}

fn classify_extension(rel_path: &str) -> FileKind {
    let lower = rel_path.to_ascii_lowercase();
    let basename = lower.rsplit('/').next().unwrap_or("");
    // 拡張子: '.' で最後に分割。 ただし basename 自体が "." で始まる場合 (dotfile) は
    // 拡張子なし扱い。 例: ".gitignore" は ext="" basename=".gitignore"
    let ext = match basename.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e,
        _ => "",
    };

    match ext {
        "md" | "markdown" => FileKind::Markdown,
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "toml" | "json" | "yaml" | "yml"
        | "txt" | "py" | "go" | "rb" | "sh" | "zsh" | "bash" | "css" | "scss" | "html" | "htm"
        | "kdl" | "lock" | "sql" | "nix" | "swift" | "kt" | "java" | "c" | "h" | "cpp" | "hpp"
        | "vue" | "svelte" | "ini" | "conf" | "env" | "fish" | "ps1" | "bat" | "cmd" => {
            FileKind::Text
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => FileKind::Image,
        "pdf" => FileKind::Unsupported("pdf (v1 未対応)".into()),
        _ => {
            // 拡張子なしの特殊ファイル
            match basename {
                "dockerfile" | "makefile" | "rakefile" | "gemfile" | "vagrantfile" | "license"
                | "readme" | "changelog" | "authors" | "contributors" | "notice" | ".gitignore"
                | ".gitattributes" | ".dockerignore" | ".npmignore" | ".env" => FileKind::Text,
                _ if ext.is_empty() => FileKind::Unsupported("unknown (no extension)".into()),
                _ => FileKind::Unsupported(format!("unknown (.{ext})")),
            }
        }
    }
}

fn has_nul_in_prefix(bytes: &[u8]) -> bool {
    bytes.iter().take(4096).any(|&b| b == 0)
}

/// `rel_path` が workdir 配下に閉じている (= traversal / 絶対パス / hidden file 無し) ことを確認。
///
/// hidden file (`.foo` / `.env` 等) は `list_entries` が `WalkBuilder.hidden(true)` で
/// 除外するのと **同じ policy** を `read_file` 側でも適用する。 list policy と open policy
/// が乖離していると IPC 直叩きで `.env` 等を読めてしまうため (moody-blues レビュー指摘)、
/// component 単位で先頭 `.` を含むものを reject する。 `.gitignore` 等を将来 user 操作で
/// 開けるようにする場合は、 ここで明示的 allow-list を作るのが筋。
fn safe_rel_path(rel: &str) -> Option<PathBuf> {
    let path = Path::new(rel);
    if path.is_absolute() {
        return None;
    }
    for c in path.components() {
        let Component::Normal(name) = c else {
            return None; // .. / root / prefix / curdir を弾く
        };
        let n = name.to_str()?;
        if n.starts_with('.') {
            return None; // hidden file / dir 一律 reject (list policy と整合)
        }
    }
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path.to_path_buf())
}

// =============================================================================
// tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(dir: &Path, rel: &str, body: &[u8]) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, body).unwrap();
    }

    fn rels(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.rel_path.as_str()).collect()
    }

    #[test]
    fn list_picks_up_basic_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "README.md", b"hello");
        touch(root, "Cargo.toml", b"[package]");
        touch(root, "src/main.rs", b"fn main() {}");
        let (entries, truncated) = list_entries(root);
        assert!(!truncated);
        let names = rels(&entries);
        assert!(names.contains(&"README.md"));
        assert!(names.contains(&"Cargo.toml"));
        assert!(names.contains(&"src/main.rs"));
        // dir も entry に出る
        assert!(names.contains(&"src"));
    }

    #[test]
    fn list_respects_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // git 認識のための .git dir (hidden で walk から除外されるが gitignore は読まれる)
        fs::create_dir_all(root.join(".git")).unwrap();
        touch(root, ".gitignore", b"secret.txt\nlogs/\n");
        touch(root, "secret.txt", b"shh");
        touch(root, "logs/app.log", b"log");
        touch(root, "keep.txt", b"ok");
        let (entries, _) = list_entries(root);
        let names = rels(&entries);
        assert!(names.contains(&"keep.txt"));
        assert!(
            !names.contains(&"secret.txt"),
            "gitignore された file が漏れた"
        );
        assert!(
            !names.iter().any(|p| p.starts_with("logs")),
            "gitignore された dir が漏れた"
        );
    }

    #[test]
    fn list_excludes_hardcoded_blocked_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // .gitignore 無しでも target / node_modules は post-filter で消える
        touch(root, "target/debug/foo", b"x");
        touch(root, "node_modules/pkg/index.js", b"x");
        touch(root, "src/main.rs", b"fn main() {}");
        let (entries, _) = list_entries(root);
        let names = rels(&entries);
        assert!(
            !names.iter().any(|p| p.starts_with("target")),
            "target/ が除外されていない"
        );
        assert!(
            !names.iter().any(|p| p.starts_with("node_modules")),
            "node_modules/ が除外されていない"
        );
        assert!(names.contains(&"src/main.rs"));
    }

    #[test]
    fn list_excludes_hidden_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, ".DS_Store", b"x");
        touch(root, ".hidden/foo", b"x");
        touch(root, "visible.txt", b"x");
        let (entries, _) = list_entries(root);
        let names = rels(&entries);
        assert!(names.contains(&"visible.txt"));
        assert!(!names.contains(&".DS_Store"));
        assert!(!names.iter().any(|p| p.starts_with(".hidden")));
    }

    #[test]
    fn list_truncates_at_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..20 {
            touch(root, &format!("f{i:02}.txt"), b"x");
        }
        let (entries, truncated) = list_entries_with_limit(root, 5);
        assert_eq!(entries.len(), 5);
        assert!(truncated);
    }

    #[test]
    fn classify_handles_dotfiles_and_unknowns() {
        assert!(matches!(classify_extension(".gitignore"), FileKind::Text));
        assert!(matches!(classify_extension("Dockerfile"), FileKind::Text));
        assert!(matches!(
            classify_extension("README.md"),
            FileKind::Markdown
        ));
        assert!(matches!(
            classify_extension("foo.unknown_ext"),
            FileKind::Unsupported(_)
        ));
        assert!(matches!(
            classify_extension("noext"),
            FileKind::Unsupported(_)
        ));
    }

    // ===== read_file（code pane の内容表示）=====
    //
    // ⚠️ open_file（board 向け render 済み content）との違いが仕様:
    // markdown も **raw source** で返す。error は `{"error"}` 1 形。

    #[test]
    fn read_markdown_returns_raw_text_not_rendered() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "doc.md", b"# Title\n\nbody");
        let v = read_file(tmp.path(), "doc.md");
        // render せずソースのまま（open_file は `markdown` variant にするのと対比）
        assert_eq!(v["text"], serde_json::json!("# Title\n\nbody"));
        assert!(v.get("markdown").is_none());
        assert!(v.get("error").is_none());
    }

    #[test]
    fn read_rust_returns_text() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "src/main.rs", b"fn main() {}\n");
        let v = read_file(tmp.path(), "src/main.rs");
        assert_eq!(v["text"], serde_json::json!("fn main() {}\n"));
    }

    #[test]
    fn read_image_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "logo.png", b"\x89PNG");
        let v = read_file(tmp.path(), "logo.png");
        assert!(v.get("text").is_none());
        assert!(v["error"].as_str().unwrap().contains("image"));
    }

    #[test]
    fn read_rejects_traversal_and_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "ok.txt", b"x");
        touch(tmp.path(), ".env", b"SECRET=1");
        assert!(read_file(tmp.path(), "../ok.txt").get("error").is_some());
        assert!(read_file(tmp.path(), "/etc/passwd").get("error").is_some());
        // hidden は list に出ないが、rel_path 直叩き（IPC 偽造）でも読めないこと
        assert!(read_file(tmp.path(), ".env").get("error").is_some());
    }

    #[test]
    fn read_too_large_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let big = vec![b'a'; (MAX_TEXT_BYTES + 1) as usize];
        touch(tmp.path(), "big.txt", &big);
        let v = read_file(tmp.path(), "big.txt");
        assert!(v["error"].as_str().unwrap().contains("too large"));
    }

    #[test]
    fn read_binary_with_text_extension_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "fake.txt", b"\x00\x01\x02");
        let v = read_file(tmp.path(), "fake.txt");
        assert!(v["error"].as_str().unwrap().contains("NUL"));
    }
}
