//! `vp lpd8` コマンドの実行ロジック

use anyhow::Result;

use crate::Lpd8Commands;

/// `vp lpd8` を実行
pub fn execute(cmd: Lpd8Commands) -> Result<()> {
    match cmd {
        Lpd8Commands::Write { port, program } => {
            if !(1..=4).contains(&program) {
                eprintln!("✗ プログラム番号は1-4の範囲で指定してください");
                std::process::exit(1);
            }
            println!("LPD8 Program {} にVP設定を書き込み中...", program);
            let vp_program = crate::midi::lpd8::Program::vp_default();
            let sysex = vp_program.to_sysex(program - 1); // 0-indexed

            match crate::midi::send_sysex(Some(&port), &sysex) {
                Ok(()) => {
                    println!("✓ VP設定をLPD8 Program {} に書き込みました", program);
                    println!();
                    println!("PAD設定:");
                    println!("  PAD 1-4 (Note 36-39): プロジェクト切り替え (緑LED)");
                    println!("  PAD 5   (Note 40):    チャットキャンセル (赤LED)");
                    println!("  PAD 6   (Note 41):    セッションリセット (橙LED)");
                    println!("  PAD 7-8 (Note 42-43): 未割当");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("✗ 書き込みエラー: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Lpd8Commands::Read { port, program } => {
            if !(1..=4).contains(&program) {
                eprintln!("✗ プログラム番号は1-4の範囲で指定してください");
                std::process::exit(1);
            }
            println!("LPD8 Program {} の読み取りは未実装です", program);
            println!("(SysExリクエスト送信後の応答受信が必要)");
            // TODO: Send request and wait for response via MidiInput
            let _ = port; // suppress warning
            Ok(())
        }
        Lpd8Commands::Switch { program, port } => {
            if !(1..=4).contains(&program) {
                eprintln!("✗ プログラム番号は1-4の範囲で指定してください");
                std::process::exit(1);
            }
            println!("LPD8をProgram {} に切り替え中...", program);
            let sysex = crate::midi::lpd8::set_active_program(program - 1);

            match crate::midi::send_sysex(Some(&port), &sysex) {
                Ok(()) => {
                    println!("✓ LPD8をProgram {} に切り替えました", program);
                    Ok(())
                }
                Err(e) => {
                    eprintln!("✗ 切り替えエラー: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Lpd8Commands::Ports => {
            crate::midi::print_output_ports();
            Ok(())
        }
    }
}
