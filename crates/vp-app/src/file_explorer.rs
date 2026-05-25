//! Sidebar File Explorer (overlay picker) — lane workdir walk + ファイル open
//!
//! ## 役割
//!
//! `files:list` / `files:open` IPC (`schema/vp-sidebar.kdl` 参照) の Rust 側実装。
//! `list_entries` で lane workdir 配下のファイルツリーを `.gitignore` を尊重して
//! 列挙し、 `open_file` で選択ファイルを Canvas (Paisley Park) 向けの JSON content
//! shape (`canvas-handler.ts` の `ShowMessage::content` と一致) に変換する。
//!
//! ## 設計判断
//!
//! - **walk は同期実行を caller が thread に逃す**: I/O blocking のため
//!   wry/tao の main thread からは呼ばず、 caller (event loop) で
//!   `thread::Builder::spawn` してから呼ぶ。 結果は `AppEvent` で push back。
//! - **画像は `Content::Html` ルート**: `protocol/messages.rs` の `ImageBase64` variant は
//!   既存 `canvas-handler.ts:40-60` で skip されているため、 base64 化した `<img>` を
//!   sandbox iframe (PP `html` mode) で表示する path を採る。 handler 改修不要。
//! - **path traversal 防御**: `open_file` は `rel_path` を `Component::Normal` のみで
//!   構成されていることを確認し、 `..` / 絶対パスを弾く。
//! - **巨大ファイル**: text 1 MiB / image 8 MiB を超えたら `> Unsupported` プレースホルダ
//!   markdown に降格。 walk は 20,000 件で truncate して `truncated: true` flag。
//! - **vp-app crate から vantage-point crate に依存させない**: `Content` enum を直接
//!   使わず、 caller が読める JSON object (`{"markdown":...}` 等) を組み立てて返す。

use base64::Engine;
use ignore::WalkBuilder;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

/// walk の最大エントリ数。 超過したら truncate + flag。
pub const DEFAULT_LIST_LIMIT: usize = 20_000;
/// text / code として開ける単一ファイルの最大サイズ。
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
/// 画像として base64 化して載せる最大サイズ。 JSON が 1.33x 膨らむのを考慮。
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
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

/// 指定ファイルを開いて Canvas (PP) 向け JSON content を返す。
///
/// 戻り値 shape は `canvas-handler.ts:22-28` の `ShowMessage::content` と同じ:
/// `{ markdown }` | `{ log }` | `{ html }` のいずれかを 1 つ含む object。
pub fn open_file(workdir: &Path, rel_path: &str) -> serde_json::Value {
    let safe_rel = match safe_rel_path(rel_path) {
        Some(p) => p,
        None => return placeholder("invalid path (traversal or absolute)", rel_path),
    };
    let full = workdir.join(&safe_rel);

    let meta = match std::fs::metadata(&full) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return placeholder("not a regular file", rel_path),
        Err(e) => return placeholder(&format!("metadata error: {e}"), rel_path),
    };

    let kind = classify_extension(rel_path);
    let cap = if matches!(kind, FileKind::Image) {
        MAX_IMAGE_BYTES
    } else {
        MAX_TEXT_BYTES
    };
    if meta.len() > cap {
        return placeholder(
            &format!("file too large ({} bytes, max {})", meta.len(), cap),
            rel_path,
        );
    }

    let bytes = match std::fs::read(&full) {
        Ok(b) => b,
        Err(e) => return placeholder(&format!("read failed: {e}"), rel_path),
    };

    match kind {
        FileKind::Markdown => match std::str::from_utf8(&bytes) {
            Ok(s) => json_markdown(s),
            Err(_) => placeholder("non-utf8 markdown", rel_path),
        },
        FileKind::Text => {
            if has_nul_in_prefix(&bytes) {
                placeholder("binary content (NUL byte detected)", rel_path)
            } else {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                json_log(&text)
            }
        }
        FileKind::Image => {
            let mime = image_mime_for(rel_path);
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let title = html_escape_minimal(rel_path);
            let html = format!(
                "<!doctype html><html><head><title>{title}</title></head>\
                <body style=\"margin:0;display:flex;align-items:center;justify-content:center;\
                background:#0a0a0a;min-height:100vh\">\
                <img src=\"data:{mime};base64,{b64}\" \
                style=\"max-width:100%;max-height:100vh;object-fit:contain\" />\
                </body></html>"
            );
            json_html(&html)
        }
        FileKind::Unsupported(reason) => placeholder(&reason, rel_path),
    }
}

// =============================================================================
// internals
// =============================================================================

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

fn image_mime_for(rel_path: &str) -> &'static str {
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".bmp") {
        "image/bmp"
    } else if lower.ends_with(".ico") {
        "image/x-icon"
    } else {
        "application/octet-stream"
    }
}

fn has_nul_in_prefix(bytes: &[u8]) -> bool {
    bytes.iter().take(4096).any(|&b| b == 0)
}

/// 文字列を HTML attr / title 用に最小エスケープ (data URL 内に持ち込まない用途のみ)。
fn html_escape_minimal(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `rel_path` が workdir 配下に閉じている (= traversal / 絶対パス無し) ことを確認。
fn safe_rel_path(rel: &str) -> Option<PathBuf> {
    let path = Path::new(rel);
    if path.is_absolute() {
        return None;
    }
    for c in path.components() {
        if !matches!(c, Component::Normal(_)) {
            return None;
        }
    }
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path.to_path_buf())
}

fn json_markdown(text: &str) -> serde_json::Value {
    serde_json::json!({ "markdown": text })
}

fn json_log(text: &str) -> serde_json::Value {
    serde_json::json!({ "log": text })
}

fn json_html(html: &str) -> serde_json::Value {
    serde_json::json!({ "html": html })
}

fn placeholder(reason: &str, rel_path: &str) -> serde_json::Value {
    // ` をエスケープして markdown code-span から抜けにくくする
    let safe_path = rel_path.replace('`', "'");
    let body = format!(
        "> **Unsupported**: `{safe_path}`\n>\n> Reason: {reason}\n>\n> v1 では preview しません。"
    );
    json_markdown(&body)
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
    fn open_markdown_returns_markdown_variant() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "doc.md", b"# Title\n\nbody");
        let v = open_file(tmp.path(), "doc.md");
        assert_eq!(v["markdown"], serde_json::json!("# Title\n\nbody"));
        assert!(v.get("log").is_none());
        assert!(v.get("html").is_none());
    }

    #[test]
    fn open_rust_returns_log_variant() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "src/main.rs", b"fn main() {}");
        let v = open_file(tmp.path(), "src/main.rs");
        assert_eq!(v["log"], serde_json::json!("fn main() {}"));
    }

    #[test]
    fn open_image_returns_html_with_data_url() {
        let tmp = tempfile::tempdir().unwrap();
        // 最小 1x1 PNG (8-byte signature + IHDR + IDAT + IEND の simplified bytes)
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
        ];
        touch(tmp.path(), "pic.png", png_bytes);
        let v = open_file(tmp.path(), "pic.png");
        let html = v["html"].as_str().expect("html field");
        assert!(html.contains("data:image/png;base64,"));
        assert!(html.contains("<img"));
    }

    #[test]
    fn open_pdf_returns_unsupported_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "spec.pdf", b"%PDF-1.4");
        let v = open_file(tmp.path(), "spec.pdf");
        let md = v["markdown"].as_str().expect("markdown field");
        assert!(md.contains("Unsupported"));
        assert!(md.contains("pdf"));
    }

    #[test]
    fn open_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "ok.txt", b"x");
        let v = open_file(tmp.path(), "../etc/passwd");
        let md = v["markdown"].as_str().expect("markdown field");
        assert!(
            md.contains("Unsupported"),
            "traversal を unsupported に降格していない"
        );
    }

    #[test]
    fn open_rejects_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let v = open_file(tmp.path(), "/etc/passwd");
        let md = v["markdown"].as_str().expect("markdown field");
        assert!(md.contains("Unsupported"));
    }

    #[test]
    fn open_binary_with_text_extension_falls_back_to_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        // .txt 拡張子だが先頭に NUL byte (binary)
        let mut bytes = vec![0x00, 0x01, 0x02, 0x03];
        bytes.extend_from_slice(b"text after nul");
        touch(tmp.path(), "binary.txt", &bytes);
        let v = open_file(tmp.path(), "binary.txt");
        let md = v["markdown"].as_str().expect("markdown field");
        assert!(md.contains("Unsupported"));
        assert!(md.contains("binary") || md.contains("NUL"));
    }

    #[test]
    fn open_text_too_large_falls_back_to_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        // 1 MiB + 1 byte
        let big = vec![b'a'; (MAX_TEXT_BYTES as usize) + 1];
        touch(tmp.path(), "big.txt", &big);
        let v = open_file(tmp.path(), "big.txt");
        let md = v["markdown"].as_str().expect("markdown field");
        assert!(md.contains("too large"));
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
}
