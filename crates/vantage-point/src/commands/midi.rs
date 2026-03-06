//! `vp midi` コマンドの実行ロジック
//!
//! MIDI入力モニタリングとLPD8コントローラー設定を統合管理する。

use anyhow::Result;
use clap::Subcommand;

/// MIDI サブコマンド
#[derive(Subcommand)]
pub enum MidiCommands {
    /// MIDI入力モニタリング開始
    Monitor {
        /// 接続するMIDIポート番号
        #[arg(short, long)]
        port: Option<usize>,
        /// アクション送信先のStandポート
        #[arg(short = 'P', long, default_value = "33000")]
        process_port: u16,
    },
    /// 利用可能なMIDI入力ポート一覧
    Ports,
    /// LPD8コントローラー設定
    #[command(subcommand)]
    Lpd8(Lpd8Commands),
}

/// LPD8 サブコマンド
#[derive(Subcommand)]
pub enum Lpd8Commands {
    /// VP用設定をLPD8に書き込む
    Write {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "LPD8")]
        port: String,
        /// 書き込み先プログラム番号（1-4）
        #[arg(short, long, default_value = "1")]
        program: u8,
    },
    /// アクティブプログラムを切り替える
    Switch {
        /// プログラム番号（1-4）
        program: u8,
        /// MIDIポート名のパターン
        #[arg(long, default_value = "LPD8")]
        port: String,
    },
    /// 利用可能なMIDI出力ポート一覧
    Ports,
}

/// `vp midi` を実行
pub fn execute(cmd: MidiCommands) -> Result<()> {
    match cmd {
        MidiCommands::Monitor { port, process_port } => {
            let mut config = crate::midi::MidiConfig::default();
            config
                .note_actions
                .insert(36, crate::midi::MidiAction::OpenWebUI { port: None });
            config
                .note_actions
                .insert(37, crate::midi::MidiAction::CancelChat { port: None });
            config
                .note_actions
                .insert(38, crate::midi::MidiAction::ResetSession { port: None });

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::midi::run_midi_interactive(
                port,
                config,
                process_port,
            ))
        }
        MidiCommands::Ports => {
            crate::midi::print_ports();
            Ok(())
        }
        MidiCommands::Lpd8(lpd8_cmd) => execute_lpd8(lpd8_cmd),
    }
}

/// LPD8 サブコマンドを実行
fn execute_lpd8(cmd: Lpd8Commands) -> Result<()> {
    match cmd {
        Lpd8Commands::Write { port, program } => {
            if !(1..=4).contains(&program) {
                eprintln!("プログラム番号は1-4の範囲で指定してください");
                std::process::exit(1);
            }
            println!("LPD8 Program {} にVP設定を書き込み中...", program);
            let vp_program = crate::midi::lpd8::Program::vp_default();
            let sysex = vp_program.to_sysex(program - 1);

            match crate::midi::send_sysex(Some(&port), &sysex) {
                Ok(()) => {
                    println!("VP設定をLPD8 Program {} に書き込みました", program);
                    println!();
                    println!("PAD設定:");
                    println!("  PAD 1-4 (Note 36-39): プロジェクト切り替え (緑LED)");
                    println!("  PAD 5   (Note 40):    チャットキャンセル (赤LED)");
                    println!("  PAD 6   (Note 41):    セッションリセット (橙LED)");
                    println!("  PAD 7-8 (Note 42-43): 未割当");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("書き込みエラー: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Lpd8Commands::Switch { program, port } => {
            if !(1..=4).contains(&program) {
                eprintln!("プログラム番号は1-4の範囲で指定してください");
                std::process::exit(1);
            }
            println!("LPD8をProgram {} に切り替え中...", program);
            let sysex = crate::midi::lpd8::set_active_program(program - 1);

            match crate::midi::send_sysex(Some(&port), &sysex) {
                Ok(()) => {
                    println!("LPD8をProgram {} に切り替えました", program);
                    Ok(())
                }
                Err(e) => {
                    eprintln!("切り替えエラー: {}", e);
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
