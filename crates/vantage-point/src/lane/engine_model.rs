//! model 語彙の検証と既定解決（純関数のみ）
//!
//! ⚠️ **旧 per-lane file store（`engine_models/<repo>__<lane>`）は 2026-07-27 に退役** —
//! model の SSOT は registry の [`super::session_registry::SessionEntry::model`]（session
//! 紐づけ、mako 裁定。doc 50 session=Pane で 1 lane 多 session になり、lane 単位は旧前提に
//! なった）。旧 file は migration せず初期化（doc 54 §8.1 — 読み手ゼロで自然消滅）。
//!
//! 本 module に残るのは model **語彙**の共有部だけ:
//! - [`is_valid_model`]: `--model '<alias>'` への injection 防壁（registry の write 側
//!   [`super::session_registry::set_model_in`] と repo 入口の検証が共用）
//! - [`resolve_default`]: 「明示 > config `default-lane-model` > 無記録」の既定規則
//!   （lane 作成の全経路が共有）

/// model 名として受理する形式（`--model` 引数に渡るため保守的に絞る）。
///
/// ## ⚠️ これは本物の injection 防壁（飾りではない）
///
/// [`crate::repo::agent_spawner`] の `claude_command` が `--model {} ` と **引用符なし**で
/// shell 文字列に埋め、それを login shell へ type-ahead 注入する。空白 / shell metachar /
/// glob（`*?[]`）/ `$` / backtick / 先頭 `~` が通ると任意コマンド実行になる。
/// 先頭 `-` を弾くのは `--model` の値が別の flag として解釈される余地を消すため。
///
/// ## `/` を許す理由（2026-08-26、vpcode dogfood で発覚）
///
/// 元の charset は **claude の語彙だけ**（`claude-opus-4-8` = 英数 + `.-_`）を想定していた。
/// ところが OpenAI 互換世界の model id は **provider 名前空間つき**が標準
/// （`openai/gpt-oss-20b` / `qwen/qwen3-coder-30b` — LM Studio / OpenRouter 等）。
/// そのため vpcode の model 切替は catalog の全候補が弾かれ **100% 失敗**していた
/// （spawn 側の catalog 先頭 fallback が「動いてはいる」状態を作り、**壊れていることを
/// 隠していた** — 常に先頭 model で走る stable-wrong）。
///
/// `/` の追加は shell 的に安全: unquoted な単語の中の `/` は metachar でなく、
/// 単独では glob も展開も起こさない（展開に要る `*?[]~$` は引き続き不許可）。
/// **model 値は path の組み立てに使われない**（file 名にしていた旧 per-lane store は
/// 2026-07-27 に退役 — 上の module doc）ので traversal 面も増えない。
pub fn is_valid_model(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '/')
}

/// 明示 model（Some=優先）と既定（config knob）から、session に記録する実効 model を返す。
///
/// **None = 記録しない**（doc 54 §8-11、mako 2026-07-25「Opus のところはユーザ設定に任せる」）:
/// 明示指定 > VP config `default-lane-model` > **無記録** — 記録が無ければ
/// `--model` は注入されず、**engine 側の user 既定**（claude なら ~/.claude の設定）が効く。
/// 旧実装は未設定時に Opus を強制 record しており、user の claude 既定を上書きしていた。
/// sub 追加の全経路（mcp / cli / sidebar）が共有する解決規則。純粋 = テスト可能。
pub fn resolve_default(explicit: Option<&str>, config_default: Option<&str>) -> Option<String> {
    explicit
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .or(config_default)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_default_prefers_explicit_then_falls_back() {
        // 明示指定が最優先
        assert_eq!(
            resolve_default(Some("claude-sonnet-5"), Some("claude-opus-4-8")),
            Some("claude-sonnet-5".to_string())
        );
        // 未指定は config knob へ
        assert_eq!(
            resolve_default(None, Some("claude-opus-4-8")),
            Some("claude-opus-4-8".to_string())
        );
        // doc 54 §8-11: 両方未指定は None = 記録しない（engine 側の user 既定に委ねる。
        // 旧「Opus 強制」の再演をここで塞ぐ）
        assert_eq!(resolve_default(None, None), None);
        assert_eq!(resolve_default(Some("   "), None), None);
        // 空白 / 空文字は config knob へ（picker の '' や whitespace を default 扱い）
        assert_eq!(
            resolve_default(Some("   "), Some("claude-opus-4-8")),
            Some("claude-opus-4-8".to_string())
        );
        assert_eq!(
            resolve_default(Some(""), Some("claude-opus-4-8")),
            Some("claude-opus-4-8".to_string())
        );
        // 明示の前後空白は trim される
        assert_eq!(
            resolve_default(Some(" claude-fable-5-1 "), Some("claude-opus-4-8")),
            Some("claude-fable-5-1".to_string())
        );
    }

    /// `--model` 引数への injection 防壁（registry write 側と repo 入口が共用する規則）。
    #[test]
    fn rejects_garbage() {
        assert!(is_valid_model("claude-opus-4-8"));
        assert!(is_valid_model("claude-haiku-4-5-20251001"));
        assert!(!is_valid_model("opus --dangerous"));
        assert!(!is_valid_model(""));
        // 先頭 `-` = `--model` の値が別 flag として解釈される余地
        assert!(!is_valid_model("--resume"));
        assert!(!is_valid_model("-x"));
        assert!(!is_valid_model("op us;rm -rf"));
    }

    /// provider 名前空間つき id（OpenAI 互換世界の標準形）を受理する。
    ///
    /// これを弾いていたため vpcode の model 切替は catalog の**全候補**が失敗していた
    /// （2026-08-26 dogfood で発覚 — spawn の catalog 先頭 fallback が「常に先頭 model で
    /// 動く」stable-wrong を作り、壊れていることを隠していた）。engine が増えるたびに
    /// 語彙も増えるので、**実在する id を literal で置いて**追加時に気づける形にする。
    #[test]
    fn accepts_namespaced_ids_but_still_blocks_shell_metachars() {
        for id in [
            "openai/gpt-oss-20b",
            "qwen/qwen3-coder-30b",
            "qwen/qwen2.5-coder-14b",
        ] {
            assert!(is_valid_model(id), "namespaced id を弾いている: {id}");
        }
        // `/` を足しても shell 防壁は無傷（unquoted 埋め込みで展開・分割が起きる文字）
        assert!(!is_valid_model("a/b c"), "空白 = 単語分割");
        assert!(!is_valid_model("a/*"), "glob");
        assert!(!is_valid_model("a/$(id)"), "コマンド置換");
        assert!(!is_valid_model("a/`id`"), "backtick");
        assert!(!is_valid_model("~/x"), "先頭 ~ = home 展開");
        assert!(!is_valid_model("/a; rm -rf /"), "; = コマンド区切り");
    }
}
