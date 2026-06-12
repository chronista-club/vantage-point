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
        /// アクション送信先のWorld daemon ポート (PR-α-3 / VP-113 で 33000 → 32000 に変更、
        /// MidiCapability が World 階層に移管済)。 旧 `--process-port` flag は alias 維持。
        #[arg(short = 'P', long, alias = "process-port", default_value = "32000")]
        world_port: u16,
    },
    /// 利用可能なMIDI入力ポート一覧
    Ports,
    /// LPD8コントローラー設定
    #[command(subcommand)]
    Lpd8(Lpd8Commands),
    /// X-Touch（MCU mode）操作
    #[command(subcommand)]
    Xtouch(XtouchCommands),
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
    /// 実機 smoke テスト（DeviceProfile 経由で pad に lane 色を投影）
    Demo {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "LPD8")]
        port: String,
    },
}

/// X-Touch サブコマンド
#[derive(Subcommand)]
pub enum XtouchCommands {
    /// 実機 smoke テスト（handshake → 8 strip に名前+色 → フェーダー階段）
    Demo {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "X-Touch")]
        port: String,
    },
    /// モーターフェーダーを波打たせる（連続送信の throughput 検証兼用）
    Wave {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "X-Touch")]
        port: String,
        /// 継続秒数
        #[arg(long, default_value = "20")]
        secs: u64,
    },
}

/// `vp midi` を実行
pub fn execute(cmd: MidiCommands) -> Result<()> {
    match cmd {
        MidiCommands::Monitor { port, world_port } => {
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
            rt.block_on(crate::midi::run_midi_interactive(port, config, world_port))
        }
        MidiCommands::Ports => {
            crate::midi::print_ports();
            Ok(())
        }
        MidiCommands::Lpd8(lpd8_cmd) => execute_lpd8(lpd8_cmd),
        MidiCommands::Xtouch(xtouch_cmd) => execute_xtouch(xtouch_cmd),
    }
}

/// X-Touch サブコマンドを実行
fn execute_xtouch(cmd: XtouchCommands) -> Result<()> {
    use crate::device_profile::{DeviceProfile, ParamSpec, Rgb, xtouch::XTouchProfile};

    match cmd {
        XtouchCommands::Demo { port } => {
            // scribble strip 固定 8 色（doc 21 §3）を 1 strip 1 色で一巡させる
            let demo_colors = [
                Rgb::new(0, 0, 0),       // Off
                Rgb::new(255, 0, 0),     // Red
                Rgb::new(0, 255, 0),     // Green
                Rgb::new(255, 255, 0),   // Yellow
                Rgb::new(0, 0, 255),     // Blue
                Rgb::new(255, 0, 255),   // Purple
                Rgb::new(0, 255, 255),   // Cyan
                Rgb::new(255, 255, 255), // White
            ];

            let mut profile = XTouchProfile::default();
            let mut messages = profile.handshake();
            for (i, color) in demo_colors.iter().enumerate() {
                let index = i as u8;
                messages.extend(profile.project_track(
                    index,
                    &format!("Lane {}", i + 1),
                    *color,
                    false,
                ));
                // フェーダーを 0/7 〜 7/7 の階段に（モーター動作の目視確認用）
                let spec = ParamSpec::continuous(format!("Param {}", i + 1), i as f32 / 7.0);
                messages.extend(profile.learn_parameter(index, &spec));
            }

            let count = messages.len();
            match crate::midi::send_batch(Some(&port), &messages) {
                Ok(()) => {
                    println!("X-Touch demo を送信しました（{} messages）", count);
                    println!();
                    println!("実機で確認すること:");
                    println!("  LCD 上段: Lane 1〜Lane 8 の表示");
                    println!("  LCD 下段: Param 1〜Param 8 の表示");
                    println!("  strip 色: Off→Red→Green→Yellow→Blue→Purple→Cyan→White");
                    println!("  フェーダー: 左から右へ階段状に上昇");
                    println!("  V-Pot ring: 値に応じた塗り表示");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("送信エラー: {}", e);
                    eprintln!("ポート確認: vp midi ports でポート一覧を表示できます");
                    std::process::exit(1);
                }
            }
        }
        XtouchCommands::Wave { port, secs } => {
            use crate::device_profile::xtouch::fader_position;
            use std::f32::consts::TAU;

            let (mut conn, port_name) = crate::midi::open_output(Some(&port))?;
            println!(
                "X-Touch wave 開始: {}（{} 秒、Ctrl-C で中断）",
                port_name, secs
            );

            let start = std::time::Instant::now();
            let frame = std::time::Duration::from_millis(33); // 約 30fps
            while start.elapsed().as_secs() < secs {
                let t = start.elapsed().as_secs_f32();
                for i in 0..8u8 {
                    // 0.4Hz の sine 波が左から右へ strip を流れる（strip 間で 1/8 周期ずらす）
                    let phase = t * TAU * 0.4 - (i as f32) * (TAU / 8.0);
                    let value = 0.5 + 0.5 * phase.sin();
                    conn.send(&fader_position(i, value))?;
                }
                std::thread::sleep(frame);
            }

            // 終了時は全フェーダーを下げて静止させる
            for i in 0..8u8 {
                conn.send(&fader_position(i, 0.0))?;
            }
            // CoreMIDI の非同期送信が流れ切るのを待ってから接続を閉じる
            std::thread::sleep(std::time::Duration::from_millis(100));
            println!("wave 終了（全フェーダーを 0 に戻しました）");
            Ok(())
        }
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
        Lpd8Commands::Demo { port } => {
            use crate::device_profile::{DeviceProfile, Rgb, lpd8::Lpd8Profile};

            // mk2 はフル RGB LED — 虹 8 色を pad に投影して色再現を確認する
            let demo_colors = [
                Rgb::new(255, 0, 0),     // Red
                Rgb::new(255, 128, 0),   // Orange
                Rgb::new(255, 255, 0),   // Yellow
                Rgb::new(0, 255, 0),     // Green
                Rgb::new(0, 255, 255),   // Cyan
                Rgb::new(0, 0, 255),     // Blue
                Rgb::new(160, 0, 255),   // Purple
                Rgb::new(255, 255, 255), // White
            ];

            let mut profile = Lpd8Profile::default();
            let mut messages = profile.handshake();
            for (i, color) in demo_colors.iter().enumerate() {
                messages.extend(profile.project_track(
                    i as u8,
                    &format!("Lane {}", i + 1),
                    *color,
                    false,
                ));
            }

            let count = messages.len();
            match crate::midi::send_batch(Some(&port), &messages) {
                Ok(()) => {
                    println!("LPD8 demo を送信しました（{} messages）", count);
                    println!();
                    println!("実機で確認すること:");
                    println!("  PAD 1-8 が虹色: Red→Orange→Yellow→Green→Cyan→Blue→Purple→White");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("送信エラー: {}", e);
                    eprintln!("ポート確認: vp midi ports でポート一覧を表示できます");
                    std::process::exit(1);
                }
            }
        }
    }
}
