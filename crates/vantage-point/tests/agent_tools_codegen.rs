//! agent-tool codegen の driver test（rebuild Epic L2 第二手、doc 27 §5-1/§5-2）。
//!
//! `schema/vp-agent.kdl` を SSOT に rmcp tool 定義を生成し
//! `src/generated/agent_tools.rs` に書き出す:
//!   - params struct（#[derive(Serialize, Deserialize, JsonSchema)]）
//!   - #[tool_router(router = agent_tool_router)] impl VantageMcp { #[tool] ... }
//!
//! sidebar_ipc_codegen.rs と同じく `cargo test -p vantage-point` で生成物が最新化される。
//! 生成 Rust が壊れていれば vantage-point lib の build が先に落ちるため、
//! 「生成物がコンパイル可能」はこの test crate がビルドされる時点で保証される。

mod agent_codegen;

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn regenerates_agent_tools_bindings() {
    let root = manifest_dir();
    let schema_path = root.join("schema/vp-agent.kdl");
    let schema_src = std::fs::read_to_string(&schema_path)
        .unwrap_or_else(|e| panic!("schema 読み込み失敗 {}: {e}", schema_path.display()));

    let schema = agent_codegen::parse(&schema_src).expect("vp-agent.kdl の parse に失敗");
    let emitted = agent_codegen::emit_rust(&schema);

    // 決定的（2 回 emit してバイト一致）であることを確認。
    let again = agent_codegen::emit_rust(&agent_codegen::parse(&schema_src).unwrap());
    assert_eq!(emitted, again, "emitter の出力が非決定的");

    // 生成物は rustfmt に通してから書き出す（`cargo fmt --all --check` を通すため。
    // emitter で rustfmt 整形を完全再現するより堅牢）。invariant assert は raw emit に対して
    // 下で行うので、rustfmt 不在（stripped toolchain 等）でも emitter 検証は走る — その場合は
    // 再整形・書き出しを skip する（raw を書くと fmt-dirty になり CI を逆に壊すため、据え置き）。
    match rustfmt(&emitted) {
        Some(formatted) => write_if_changed(&root.join("src/generated/agent_tools.rs"), &formatted),
        None => eprintln!(
            "⚠ rustfmt が見つからないため生成物の再整形・書き出しをスキップ（committed file は据え置き）"
        ),
    }

    // --- invariants ---------------------------------------------------------
    // wire/delegation 8 tool 全部の #[tool] メソッドと params struct が出ていること。
    for tool in [
        "wire_send",
        "wire_recv",
        "wire_thread",
        "wire_inbox",
        "wire_ack",
        "delegate",
        "complete",
        "respond",
    ] {
        assert!(
            emitted.contains(&format!("async fn {tool}(")),
            "tool {tool} のメソッドが無い"
        );
    }
    for params in [
        "WireSendParams",
        "WireRecvParams",
        "WireThreadParams",
        "WireInboxParams",
        "WireAckParams",
        "DelegateParams",
        "CompleteParams",
        "RespondParams",
    ] {
        assert!(
            emitted.contains(&format!("pub struct {params}")),
            "params struct {params} が無い"
        );
    }

    // named router macro（手書き router と合流するための別名）。
    assert!(
        emitted.contains("#[tool_router(router = agent_tool_router, vis = \"pub(crate)\")]"),
        "named tool_router macro が無い"
    );

    // body="custom"（complete / wire_recv）は手書き helper に委譲。
    assert!(emitted.contains("self.wire_recv_impl(params).await"));
    assert!(emitted.contains("self.complete_impl(params).await"));

    // method override（wire_inbox → repo method wire_unread_count）。
    assert!(emitted.contains("self.quic_call(\"wire_unread_count\", payload)"));

    // self_lane inject（from / agent / requester）。
    assert!(emitted.contains("let __self_lane = self.self_lane.from_address()?;"));

    // body フィールドの object schema 明示（client に object を送らせる救済）。
    assert!(emitted.contains("with = \"std::collections::HashMap<String, serde_json::Value>\""));
}

/// emit 出力を `rustfmt`（edition 2024）に通して整形する。
/// rustfmt が PATH に無ければ panic させず `None` を返す（呼び出し側で skip）。
fn rustfmt(src: &str) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let spawn = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawn {
        Ok(child) => child,
        // rustfmt 不在（stripped toolchain 等）— graceful skip。
        Err(_) => return None,
    };
    child
        .stdin
        .take()
        .expect("rustfmt stdin")
        .write_all(src.as_bytes())
        .expect("rustfmt stdin への書き込み失敗");
    let out = child.wait_with_output().expect("rustfmt 実行失敗");
    assert!(
        out.status.success(),
        "rustfmt が失敗: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("rustfmt 出力が非 UTF-8"))
}

/// 内容が変わったときだけ書き込む（無変更なら git status / mtime を汚さない）。
fn write_if_changed(path: &Path, content: &str) {
    if std::fs::read_to_string(path).is_ok_and(|cur| cur == content) {
        return;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("生成先ディレクトリ作成失敗");
    }
    std::fs::write(path, content)
        .unwrap_or_else(|e| panic!("生成物の書き込み失敗 {}: {e}", path.display()));
}
