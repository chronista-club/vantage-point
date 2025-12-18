//! Capability Synergy System の使用例
//!
//! このサンプルでは、Stand Capability間の連携システムの使い方を示す。
//!
//! 実行方法:
//! ```bash
//! cargo run --example capability_synergy
//! ```

use vantage_point::capability::synergy::*;

fn main() {
    println!("=== Stand Capability Synergy System Demo ===\n");

    let mut engine = SynergyEngine::new();

    // 事前定義済み能力を登録
    engine.register(midi_capability());
    engine.register(claude_agent_capability());
    engine.register(webview_capability());
    engine.register(websocket_capability());
    engine.register(session_management_capability());

    // === Demo 1: MIDI入力 + Claude Agent連携 ===
    println!("--- Demo 1: MIDI入力 + Claude Agent連携 ---");
    demo_midi_agent_synergy(&mut engine);
    println!();

    // === Demo 2: WebView + WebSocket連携 ===
    println!("--- Demo 2: WebView + WebSocket連携 ---");
    demo_webview_websocket_synergy(&mut engine);
    println!();

    // === Demo 3: 依存関係の解決 ===
    println!("--- Demo 3: 依存関係の解決 ---");
    demo_dependency_resolution(&engine);
    println!();

    // === Demo 4: 最適な組み合わせの提案 ===
    println!("--- Demo 4: 最適な組み合わせの提案 ---");
    demo_suggest_combinations(&mut engine);
    println!();

    // === Demo 5: 禁忌組み合わせ ===
    println!("--- Demo 5: 禁忌組み合わせ ---");
    demo_forbidden_combination(&mut engine);
    println!();

    // === Demo 6: カスタム能力の定義 ===
    println!("--- Demo 6: カスタム能力の定義 ---");
    demo_custom_capability(&mut engine);
    println!();
}

fn demo_midi_agent_synergy(engine: &mut SynergyEngine) {
    let analysis = engine
        .analyze("midi_input", "claude_agent")
        .expect("分析に失敗");

    println!("能力A: MIDI Input");
    println!("能力B: Claude Agent");
    println!("相性スコア: {}/100", analysis.compatibility);
    println!("依存関係充足: {}", if analysis.dependencies_met { "✓" } else { "✗" });
    println!("連携タイプ: {:?}", analysis.synergy_type);
    println!("説明: {}", analysis.description);

    // 実際の連携効果
    println!("\n実際の連携効果:");
    println!("  - MIDIパッド1押下 → プロジェクト1を開く");
    println!("  - MIDIパッド2押下 → チャット送信");
    println!("  - MIDIパッド3押下 → セッションリセット");
    println!("  - LED点灯/点滅 → Agent状態をフィードバック");
}

fn demo_webview_websocket_synergy(engine: &mut SynergyEngine) {
    let analysis = engine
        .analyze("webview_ui", "websocket_comm")
        .expect("分析に失敗");

    println!("能力A: WebView UI");
    println!("能力B: WebSocket Communication");
    println!("相性スコア: {}/100", analysis.compatibility);
    println!("依存関係充足: {}", if analysis.dependencies_met { "✓" } else { "✗" });
    println!("連携タイプ: {:?}", analysis.synergy_type);
    println!("説明: {}", analysis.description);

    println!("\n実際の連携効果:");
    println!("  - WebSocketでリアルタイムな双方向通信");
    println!("  - チャットストリーミング表示");
    println!("  - AG-UIイベント配信");
}

fn demo_dependency_resolution(engine: &SynergyEngine) {
    let deps = engine.find_dependencies("claude_agent");

    println!("Claude Agentが必要とする依存:");
    println!("  - UserInput (ユーザー入力)");
    println!();
    println!("依存を満たす能力:");
    for dep in deps {
        println!("  - {}", dep);
    }
}

fn demo_suggest_combinations(engine: &mut SynergyEngine) {
    let suggestions = engine.suggest_combinations("claude_agent", 5);

    println!("Claude Agentと相性の良い能力 (Top 5):");
    for (i, suggestion) in suggestions.iter().enumerate() {
        println!(
            "  {}. {} (スコア: {}/100)",
            i + 1,
            suggestion.capability_b,
            suggestion.compatibility
        );
        println!("     {}", suggestion.description);
    }
}

fn demo_forbidden_combination(engine: &mut SynergyEngine) {
    // 禁忌組み合わせの例: 排他制御が必要な能力同士
    let mutex_a = CapabilityMetadata::new("file_lock_a", "File Lock A")
        .with_description("ファイルAへの排他ロック")
        .provides(vec![CapabilityTag::FileSystem])
        .forbidden_with(vec!["file_lock_b".to_string()]);

    let mutex_b = CapabilityMetadata::new("file_lock_b", "File Lock B")
        .with_description("ファイルBへの排他ロック（同一リソース）")
        .provides(vec![CapabilityTag::FileSystem]);

    engine.register(mutex_a);
    engine.register(mutex_b);

    let analysis = engine
        .analyze("file_lock_a", "file_lock_b")
        .expect("分析に失敗");

    println!("能力A: File Lock A");
    println!("能力B: File Lock B");
    println!("相性スコア: {}/100", analysis.compatibility);
    println!("禁忌組み合わせ: {}", if analysis.is_forbidden { "✓" } else { "✗" });
    println!("連携タイプ: {:?}", analysis.synergy_type);
    println!("説明: {}", analysis.description);
    println!();
    println!("理由: 同一リソースへの排他ロックはデッドロックを引き起こす");
}

fn demo_custom_capability(engine: &mut SynergyEngine) {
    // カスタム能力の定義例: 音声入力能力
    let voice_input = CapabilityMetadata::new("voice_input", "Voice Input")
        .with_description("音声入力でチャットメッセージを送信")
        .provides(vec![
            CapabilityTag::UserInput,
            CapabilityTag::AudioFeedback, // エコーバック
        ])
        .requires(vec![])
        .synergizes_with(vec![
            CapabilityTag::AiAgent,
            CapabilityTag::NaturalLanguage,
        ])
        .conflicts_with(vec![
            // 音声入力と音声出力は干渉する可能性
            CapabilityTag::AudioFeedback,
        ]);

    engine.register(voice_input);

    let analysis = engine
        .analyze("voice_input", "claude_agent")
        .expect("分析に失敗");

    println!("カスタム能力: Voice Input");
    println!("連携先: Claude Agent");
    println!("相性スコア: {}/100", analysis.compatibility);
    println!("連携タイプ: {:?}", analysis.synergy_type);
    println!("説明: {}", analysis.description);
    println!();
    println!("実装イメージ:");
    println!("  1. マイクから音声入力");
    println!("  2. 音声認識でテキスト化");
    println!("  3. Claude Agentにチャット送信");
    println!("  4. Agent応答を音声合成でフィードバック");
}
