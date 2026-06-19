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
    /// ROTO-CONTROL 操作
    #[command(subcommand)]
    Roto(RotoCommands),
}

/// ROTO-CONTROL サブコマンド
#[derive(Subcommand)]
pub enum RotoCommands {
    /// 実機 smoke テスト（DAW_START → 8 track に名前+色 → 8 knob に parameter learn）
    Demo {
        /// MIDIポート名のパターン（部分一致。実機の CoreMIDI 名は "Roto-Control"）
        #[arg(long, default_value = "Roto")]
        port: String,
    },
    /// knob モーターを BPM 同期でアニメーションさせる（wave/pulse/chase/bounce の 4 パターン）
    Anim {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "Roto")]
        port: String,
        /// テンポ
        #[arg(long, default_value = "120")]
        bpm: u16,
        /// 継続秒数
        #[arg(long, default_value = "32")]
        secs: u64,
    },
    /// 入力 watch（機材操作 → 論理 ControlEvent を表示、E2 input flow の smoke）
    Watch {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "Roto")]
        port: String,
        /// 観察秒数
        #[arg(long, default_value = "30")]
        secs: u64,
    },
    /// handshake 観察（DAW_START を送り、ROTO からの応答 SysEx を hex で表示）
    Probe {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "Roto")]
        port: String,
        /// 観察秒数
        #[arg(long, default_value = "5")]
        secs: u64,
    },
    /// B2: ROTO の ← / → で vp-app の active Lane を prev/next 切替（Unison-native）
    ///
    /// transport ◄ (CC36)=prev / ► (CC37)=next（or 右 button 0/1）。現 project の local SP に
    /// QUIC 接続し、「lanes」channel で lane list を購読、`switch_lane` QUIC method で発火する。
    Control {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "Roto")]
        port: String,
        /// 接続先 TheWorld ポート（cross-project lane view の集約元）
        #[arg(long, default_value = "32000")]
        world_port: u16,
        /// 継続秒数
        #[arg(long, default_value = "600")]
        secs: u64,
    },
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
        MidiCommands::Roto(roto_cmd) => execute_roto(roto_cmd),
    }
}

/// ROTO との MIDI 接続一式（受信 connection は drop すると切れるため保持して返す）
#[allow(clippy::type_complexity)]
fn roto_open(
    port: &str,
) -> Result<(
    midir::MidiInputConnection<()>,
    std::sync::mpsc::Receiver<Vec<u8>>,
    midir::MidiOutputConnection,
    String,
)> {
    let mut midi_in = midir::MidiInput::new("vp-midi-roto")?;
    midi_in.ignore(midir::Ignore::None);
    let in_ports = midi_in.ports();
    let in_port = in_ports
        .iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .map(|name| name.contains(port))
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("No MIDI input port matching '{}'", port))?;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let conn_in = midi_in
        .connect(
            in_port,
            "vp-roto",
            move |_timestamp, bytes, _| {
                let _ = tx.send(bytes.to_vec());
            },
            (),
        )
        .map_err(|e| anyhow::anyhow!("MIDI input connect failed: {}", e))?;
    let (conn_out, port_name) = crate::midi::open_output(Some(port))?;
    Ok((conn_in, rx, conn_out, port_name))
}

/// ROTO からのメッセージへの定型応答（keepalive）。
/// hello (02 0A 02) → ping + meter threshold、0A 0C → 0A 0D（Bitwig 拡張準拠）。
/// 応答した場合 true を返す。
fn roto_autorespond(bytes: &[u8], conn_out: &mut midir::MidiOutputConnection) -> Result<bool> {
    const HELLO: [u8; 8] = [0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x02, 0xF7];
    const QUERY_0C: [u8; 8] = [0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x0C, 0xF7];
    const METER_THRESHOLD: [u8; 10] = [0xF0, 0x00, 0x22, 0x03, 0x02, 0x0C, 0x0B, 0x2F, 0x73, 0xF7];
    if bytes == HELLO {
        conn_out.send(&crate::device_profile::roto::ping())?;
        conn_out.send(&METER_THRESHOLD)?;
        return Ok(true);
    }
    if bytes == QUERY_0C {
        conn_out.send(&[0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x0D, 0xF7])?;
        return Ok(true);
    }
    Ok(false)
}

/// ROTO 受信 SysEx を人間可読ラベルに解読する（decompile の `handleRotoUpdate`
/// dispatch 準拠）。`F0 00 22 03 02 <type> <cmd> ...` の (type, cmd) を操作名に対応づける。
/// SysEx でなければ `None`。左キーの nav/mode 操作はここで識別する。
fn roto_sysex_label(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(&[0xF0, 0x00, 0x22, 0x03, 0x02]) || bytes.len() < 7 {
        return None;
    }
    let (ty, cmd) = (bytes[5], bytes[6]);
    let arg = bytes.get(7).copied().unwrap_or(0);
    // 注意: 0A 02 (hello) / 0A 0C (query) は watch では roto_autorespond が
    // 先行キャッチして届かない。テーブルは仕様書としての完全性のため両者も載せる
    let label = match (ty, cmd) {
        (0x0A, 0x02) => "general: hello/keepalive".into(),
        (0x0A, 0x06) => "general: track offset".into(),
        (0x0A, 0x09) => format!(
            "general: selectTrack (knob {} 触れ → track 選択)",
            bytes.get(8).copied().unwrap_or(0)
        ),
        (0x0A, 0x0A) => "general: transport mode へ切替".into(),
        (0x0A, 0x0C) => "general: query (→ 0D 応答)".into(),
        (0x0A, 0x0E) => "general: firmware version 通知".into(),
        (0x0A, 0x14) => "general: plugin param page (prev)".into(),
        (0x0A, 0x15) => "general: plugin param page (next)".into(),
        (0x0B, 0x01) => "plugin: plugin mode へ".into(),
        (0x0B, 0x04) => "plugin: bank navigate".into(),
        (0x0B, 0x07) => "plugin: select plugin".into(),
        (0x0B, 0x09) => "plugin: learn mode toggle".into(),
        (0x0B, 0x0B) => "plugin: activate parameter".into(),
        (0x0B, 0x0C) => "plugin: activate plugin".into(),
        (0x0B, 0x0D) => "plugin: lock device".into(),
        (0x0B, 0x10) => "plugin: select remote page".into(),
        (0x0B, 0x11) => "plugin: confirm learned".into(),
        (0x0B, 0x12) => "plugin: toggle remote page".into(),
        (0x0C, 0x01) => "mixer: update (keepalive)".into(),
        (0x0C, 0x02) => format!("mixer: track control = {}", arg),
        (0x0C, 0x05) => format!("mixer: master control = {}", arg),
        (0x0C, 0x06) => "mixer: focus track toggle".into(),
        _ => format!("? type={:02X} cmd={:02X}", ty, cmd),
    };
    Some(label)
}

/// ROTO-CONTROL サブコマンドを実行
fn execute_roto(cmd: RotoCommands) -> Result<()> {
    use crate::device_profile::{DeviceProfile, ParamSpec, Rgb, roto::RotoProfile};
    use std::time::{Duration, Instant};

    // demo / anim 共通の虹 8 色（83 色パレットへの量子化を通る）
    let demo_colors = [
        Rgb::new(255, 0, 0),     // 赤
        Rgb::new(255, 128, 0),   // 橙
        Rgb::new(255, 255, 0),   // 黄
        Rgb::new(0, 255, 0),     // 緑
        Rgb::new(0, 255, 255),   // シアン
        Rgb::new(0, 0, 255),     // 青
        Rgb::new(160, 0, 255),   // 紫
        Rgb::new(255, 255, 255), // 白
    ];

    match cmd {
        RotoCommands::Demo { port } => {
            let (_conn_in, rx, mut conn_out, port_name) = roto_open(&port)?;

            let mut profile = RotoProfile::default();
            for msg in profile.handshake() {
                conn_out.send(&msg)?;
            }
            println!("DAW_START 送信（{}）→ hello 応答ループ開始", port_name);

            // projection メッセージを準備。track 表示の枠付きバッチ
            // （track 数 → offset → 更新 × N → end detail）は profile が組むので、
            // 8 track ぶん shadow を更新して最後のバッチだけ送る
            let mut projection = Vec::new();
            for (i, color) in demo_colors.iter().enumerate() {
                projection =
                    profile.project_track(i as u8, &format!("Lane {}", i + 1), *color, false);
            }
            // knob learn は track バッチの後に（値は 0/7〜7/7 の階段）
            for i in 0..8u8 {
                let spec = ParamSpec::continuous(format!("Param {}", i + 1), i as f32 / 7.0);
                projection.extend(profile.learn_parameter(i, &spec));
            }

            // hello に応答しつつ device の通知を観察。最初の hello 応答から
            // 1.5 秒後に projection を送り、その後も keepalive を続ける（計 15 秒）
            let start = Instant::now();
            let mut first_hello: Option<Instant> = None;
            let mut projected = false;
            while start.elapsed() < Duration::from_secs(15) {
                if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
                    if roto_autorespond(&bytes, &mut conn_out)? {
                        if first_hello.is_none() {
                            println!("hello 受信 → ping + meter threshold で応答（以後毎回）");
                            first_hello = Some(Instant::now());
                        }
                    } else if bytes.first() == Some(&0xF0) {
                        // 定型応答対象外の SysEx = device 状態通知。観察用に表示
                        let hex: Vec<String> = bytes.iter().map(|b| format!("{:02X}", b)).collect();
                        println!("受信: {}", hex.join(" "));
                    }
                }
                if !projected
                    && first_hello.is_some_and(|t| t.elapsed() > Duration::from_millis(1500))
                {
                    for msg in &projection {
                        conn_out.send(msg)?;
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    println!(
                        "projection 送信（{} messages）— track 表示と knob learn を確認してください",
                        projection.len()
                    );
                    projected = true;
                }
            }
            if first_hello.is_none() {
                println!("hello が来ませんでした — ROTO の mode / 接続を確認してください");
            }
            Ok(())
        }
        RotoCommands::Anim { port, bpm, secs } => {
            use crate::device_profile::roto::knob_position;
            use std::f32::consts::{PI, TAU};

            let (_conn_in, rx, mut conn_out, port_name) = roto_open(&port)?;

            let profile_handshake = RotoProfile::default().handshake();
            for msg in profile_handshake {
                conn_out.send(&msg)?;
            }
            println!("DAW_START 送信（{}）→ 接続確立中...", port_name);

            // hello に応答しながら接続成立を待つ（最初の hello から 1.5 秒）
            let start = Instant::now();
            let mut first_hello: Option<Instant> = None;
            while start.elapsed() < Duration::from_secs(10) {
                if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
                    roto_autorespond(&bytes, &mut conn_out)?;
                    if first_hello.is_none() && bytes.first() == Some(&0xF0) {
                        first_hello = Some(Instant::now());
                    }
                }
                if first_hello.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
                    break;
                }
            }

            // 背景として track 表示（Lane 名 + 虹色）を乗せる
            let mut profile = RotoProfile::default();
            let mut backdrop = Vec::new();
            for (i, color) in demo_colors.iter().enumerate() {
                backdrop =
                    profile.project_track(i as u8, &format!("Lane {}", i + 1), *color, false);
            }
            for msg in &backdrop {
                conn_out.send(msg)?;
                std::thread::sleep(Duration::from_millis(1));
            }

            println!(
                "knob アニメーション開始: BPM {}、{} 秒（wave → pulse → chase → bounce を 8 拍ごとに切替）",
                bpm, secs
            );

            let beat_dur = 60.0 / bpm as f32;
            let pattern_names = ["WAVE", "PULSE", "CHASE", "BOUNCE"];
            let t0 = Instant::now();
            // 差分送信用 cache（knob: [hi, lo] × 8、button LED: 2 列 × 8）
            let mut last_sent = [[0xFFu8; 2]; 8];
            let mut last_button = [[0xFFu8; 8]; 2];
            let mut last_beat_index = usize::MAX;
            let mut last_pattern = usize::MAX;
            while t0.elapsed() < Duration::from_secs(secs) {
                // keepalive（hello / 0A 0C への応答を続けないと接続が切れる）
                while let Ok(bytes) = rx.try_recv() {
                    roto_autorespond(&bytes, &mut conn_out)?;
                }

                let beat = t0.elapsed().as_secs_f32() / beat_dur;
                let beat_index = beat as usize;
                let pattern = (beat_index / 8) % 4;
                let chase_pos = (beat * 2.0) as usize % 8; // 8 分音符の彗星位置

                // knob 値をパターンごとに計算（button の振付でも使う）
                let mut values = [0.0f32; 8];
                for (i, v) in values.iter_mut().enumerate() {
                    let k = i as f32;
                    *v = match pattern {
                        // wave: 1 小節（4 拍）で knob 列を一周する波
                        0 => 0.5 + 0.5 * (TAU * (beat / 4.0) - k * TAU / 8.0).sin(),
                        // pulse: 拍頭で全 knob が跳ねて 3 乗カーブで減衰
                        1 => (1.0 - beat.fract()).powi(3),
                        // chase: 8 分音符で彗星が一周（距離に応じた残光つき）
                        2 => {
                            let raw = (i as i32 - chase_pos as i32).unsigned_abs();
                            let dist = raw.min(8 - raw) as f32;
                            (1.0 - 0.35 * dist).max(0.05)
                        }
                        // bounce: 奇数/偶数 knob が 2 拍周期でシーソー
                        _ => {
                            0.5 + 0.45
                                * (TAU * beat / 2.0 + if i % 2 == 0 { 0.0 } else { PI }).sin()
                        }
                    };
                }

                // knob モーター（差分のみ）
                for (i, v) in values.iter().enumerate() {
                    let value = (16383.0 * v.clamp(0.0, 1.0)).round() as u16;
                    let hi_lo = [(value >> 7) as u8, (value & 0x7F) as u8];
                    if last_sent[i] != hi_lo {
                        for msg in knob_position(i as u8, *v) {
                            conn_out.send(&msg)?;
                        }
                        last_sent[i] = hi_lo;
                    }
                }

                // LCD: 毎拍 虹色を 1 つ回転、パターン切替時は表示名も切替
                if beat_index != last_beat_index {
                    let name_changed = pattern != last_pattern;
                    let mut batch = Vec::new();
                    for i in 0..8usize {
                        let color = demo_colors[(i + beat_index) % 8];
                        let name = if name_changed {
                            pattern_names[pattern].to_string()
                        } else {
                            format!("{} {}", pattern_names[pattern], i + 1)
                        };
                        batch = profile.project_track(i as u8, &name, color, false);
                    }
                    for msg in &batch {
                        conn_out.send(msg)?;
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    last_beat_index = beat_index;
                    last_pattern = pattern;
                }

                // button LED（CC 20–27 = 本体列 / 28–35 = transport 列、0 or 127）
                for (row, row_cache) in last_button.iter_mut().enumerate() {
                    for (i, cache) in row_cache.iter_mut().enumerate() {
                        let on = match pattern {
                            // wave: knob の山に追従（transport 列は谷）
                            0 => {
                                if row == 0 {
                                    values[i] > 0.6
                                } else {
                                    values[i] < 0.4
                                }
                            }
                            // pulse: 本体列は拍頭、transport 列は 8 分裏で点滅
                            1 => {
                                if row == 0 {
                                    beat.fract() < 0.25
                                } else {
                                    (beat * 2.0).fract() < 0.25
                                }
                            }
                            // chase: 彗星と対角の 2 点が追いかける
                            2 => {
                                if row == 0 {
                                    i == chase_pos
                                } else {
                                    i == 7 - chase_pos
                                }
                            }
                            // bounce: 奇偶 × 列で市松に交互
                            _ => (i + row) % 2 == ((beat / 2.0) as usize) % 2,
                        };
                        let v = if on { 127 } else { 0 };
                        if *cache != v {
                            let cc = if row == 0 { 20 } else { 28 } + i as u8;
                            conn_out.send(&[0xBF, cc, v])?;
                            *cache = v;
                        }
                    }
                }

                std::thread::sleep(Duration::from_millis(33)); // 約 30fps
            }

            // 終了: 全 knob を 0 に戻し、button LED を消灯
            for i in 0..8u8 {
                for msg in knob_position(i, 0.0) {
                    conn_out.send(&msg)?;
                }
                conn_out.send(&[0xBF, 20 + i, 0])?;
                conn_out.send(&[0xBF, 28 + i, 0])?;
            }
            std::thread::sleep(Duration::from_millis(100));
            println!("anim 終了（全 knob を 0 に戻しました）");
            Ok(())
        }
        RotoCommands::Watch { port, secs } => {
            use crate::device_input::{DeviceInput, roto::RotoInput};

            let (_conn_in, rx, mut conn_out, port_name) = roto_open(&port)?;

            // 入力は接続成立後に来る。demo/anim と同じ handshake シーケンスを踏む
            let mut profile = RotoProfile::default();
            for msg in profile.handshake() {
                conn_out.send(&msg)?;
            }
            println!("DAW_START 送信（{}）→ 接続確立中...", port_name);

            // 接続成立後に送る projection を用意（track 表示 + 8 knob の learn）。
            // knob は learn で active になるまで入力 CC を送らない（ccInsBlocked）ため、
            // これを送って knob を起こさないと watch で何も拾えない（実機検証 2026-06-13）。
            let mut projection = Vec::new();
            for i in 0..8u8 {
                projection = profile.project_track(
                    i,
                    &format!("Lane {}", i + 1),
                    crate::device_profile::Rgb::new(0, 200, 255),
                    false,
                );
            }
            for i in 0..8u8 {
                let spec = ParamSpec::continuous(format!("Param {}", i + 1), 0.5);
                projection.extend(profile.learn_parameter(i, &spec));
            }

            let start = Instant::now();
            let mut input = RotoInput::default();
            let mut event_count = 0usize;
            let mut activated = false;
            while start.elapsed() < Duration::from_secs(secs) {
                let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                // MIDI realtime（clock 0xF8 等の 1 byte system message）は無視
                if bytes.len() == 1 && bytes[0] >= 0xF8 {
                    continue;
                }
                // handshake/keepalive の SysEx は自動応答（接続を維持）
                if roto_autorespond(&bytes, &mut conn_out)? {
                    continue;
                }
                // 定型応答対象外の最初の SysEx（実機では firmware version 0A 0E が先着）で
                // projection を送って knob を起こす（learn しないと入力 CC が来ない）
                if !activated && bytes.first() == Some(&0xF0) {
                    activated = true;
                    for msg in &projection {
                        conn_out.send(msg)?;
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    println!(
                        "接続成立 + knob 起動 — ROTO の knob を回す / touch する / button を押すと ControlEvent が出ます"
                    );
                    continue;
                }
                // channel message を論理 ControlEvent へ
                if let Some(event) = input.parse(&bytes) {
                    event_count += 1;
                    println!("[{:>3}] {:?}", event_count, event);
                } else if let Some(label) = roto_sysex_label(&bytes) {
                    // 受信 SysEx をラベル解読（左キー nav/mode はここで識別）。
                    // mixer update (0C 01) は keepalive ノイズなので抑制
                    if !bytes.starts_with(&[0xF0, 0x00, 0x22, 0x03, 0x02, 0x0C, 0x01]) {
                        println!("      (sysex: {})", label);
                    }
                } else {
                    // SysEx 以外の未対応受信は生 hex で表示（実機の実 byte 確認用）
                    let hex: Vec<String> = bytes.iter().map(|b| format!("{:02X}", b)).collect();
                    println!("      raw: {}", hex.join(" "));
                }
            }
            println!("watch 終了（{} events）", event_count);
            Ok(())
        }
        RotoCommands::Probe { port, secs } => {
            use std::sync::mpsc;

            // 入力 port を pattern で解決（SysEx を観察するため ignore を解除する）
            let mut midi_in = midir::MidiInput::new("vp-midi-probe")?;
            midi_in.ignore(midir::Ignore::None);
            let in_ports = midi_in.ports();
            let in_port = in_ports
                .iter()
                .find(|p| {
                    midi_in
                        .port_name(p)
                        .map(|name| name.contains(&port))
                        .unwrap_or(false)
                })
                .ok_or_else(|| anyhow::anyhow!("No MIDI input port matching '{}'", port))?;

            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            let _conn_in = midi_in
                .connect(
                    in_port,
                    "vp-roto-probe",
                    move |_timestamp, bytes, _| {
                        let _ = tx.send(bytes.to_vec());
                    },
                    (),
                )
                .map_err(|e| anyhow::anyhow!("MIDI input connect failed: {}", e))?;

            // DAW_START を送って応答を観察する（doc 20 §6/§10 残課題 2 の実機検証）
            let profile = RotoProfile::default();
            crate::midi::send_batch(Some(&port), &profile.handshake())?;
            println!("DAW_START 送信 → {} 秒間 受信を観察します...", secs);

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            let mut count = 0usize;
            while std::time::Instant::now() < deadline {
                if let Ok(bytes) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
                    count += 1;
                    let hex: Vec<String> = bytes.iter().map(|b| format!("{:02X}", b)).collect();
                    println!("[{:>3}] {}", count, hex.join(" "));
                }
            }
            println!("受信 {} 件", count);
            Ok(())
        }
        RotoCommands::Control {
            port,
            world_port,
            secs,
        } => execute_roto_control(port, world_port, secs),
    }
}

/// ROTO MIDI を開き、入力 byte 列を **tokio mpsc** で渡す（async control loop 用、B2）。
/// `roto_open` の std mpsc 版に対し、midir の sync callback から `UnboundedSender::send`
/// （非 async・別スレッド可）で async ループへ bridge する。
#[allow(clippy::type_complexity)]
fn roto_open_async(
    port: &str,
) -> Result<(
    midir::MidiInputConnection<()>,
    tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    midir::MidiOutputConnection,
    String,
)> {
    let mut midi_in = midir::MidiInput::new("vp-midi-roto-control")?;
    midi_in.ignore(midir::Ignore::None);
    let in_ports = midi_in.ports();
    let in_port = in_ports
        .iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .map(|name| name.contains(port))
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("No MIDI input port matching '{}'", port))?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let conn_in = midi_in
        .connect(
            in_port,
            "vp-roto-control",
            move |_timestamp, bytes, _| {
                let _ = tx.send(bytes.to_vec());
            },
            (),
        )
        .map_err(|e| anyhow::anyhow!("MIDI input connect failed: {}", e))?;
    let (conn_out, port_name) = crate::midi::open_output(Some(port))?;
    Ok((conn_in, rx, conn_out, port_name))
}

/// local SP に Unison QUIC 接続（mcp.rs の connect_quic と同等、private 再実装）。
/// ⚠️ mcp.rs::connect_quic と trust_anchors を揃えること（PR-3 で SkipVerification →
/// InternalMeshKeypair に差し替え予定。mcp.rs を変えたらここも同期）。
async fn connect_quic_local(port: u16) -> Result<unison::ProtocolClient> {
    let addr = format!("[::1]:{}", port);
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| anyhow::anyhow!("Unison client build: {}", e))?;
    let client = unison::ProtocolClient::new(transport);
    client
        .connect(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("Unison connect {}: {}", addr, e))?;
    Ok(client)
}

/// cross-project view の 1 lane。TheWorld `list_all_lanes` の flat 化結果。
#[derive(Clone, PartialEq)]
struct RotoLane {
    /// switch_lane 送信先 SP port
    port: u16,
    /// switch_lane payload の lane token（"conductor" or performer 名）
    token: String,
    /// LCD 表示用の compact ラベル（≤13 文字、project + lane）
    label: String,
    /// 選択追跡の一意キー `"{port}:{token}"`
    key: String,
}

/// LCD 13 文字制約に収まる compact ラベルを作る。
/// project を distinguish したいので project 優先（8 文字）+ ":" + lane（4 文字）。
fn compact_lane_label(project: &str, token: &str) -> String {
    let proj: String = project.chars().take(8).collect();
    let lane: String = token.chars().take(4).collect();
    format!("{}:{}", proj, lane)
}

/// TheWorld `list_all_lanes` 応答（`{projects: [{project_name, port, lanes:[LaneInfo]}]}`）を
/// flat な `Vec<RotoLane>` に変換。
///
/// **順序は server が決める** — server は project_order (= sidebar 順) で projects を、
/// 各 project 内は lane_registry 順 (= conductor 先頭 + performer 作成順) で lanes を送る。
/// client は再ソートせず、その順序をそのまま保つ（物理 controller の位置 = sidebar の位置）。
fn parse_world_lanes(v: &serde_json::Value) -> Vec<RotoLane> {
    let Some(projects) = v.get("projects").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in projects {
        let Some(project) = p.get("project_name").and_then(|n| n.as_str()) else {
            continue;
        };
        let Some(port) = p.get("port").and_then(|n| n.as_u64()) else {
            continue;
        };
        let port = port as u16;
        let Some(lanes) = p.get("lanes").and_then(|l| l.as_array()) else {
            continue;
        };
        for lane in lanes {
            let Some(kind) = lane.get("kind").and_then(|k| k.as_str()) else {
                continue;
            };
            let token = if kind == "conductor" {
                "conductor".to_string()
            } else {
                match lane
                    .get("address")
                    .and_then(|a| a.get("name"))
                    .and_then(|n| n.as_str())
                {
                    Some(name) => name.to_string(),
                    None => continue,
                }
            };
            out.push(RotoLane {
                port,
                label: compact_lane_label(project, &token),
                key: format!("{}:{}", port, token),
                token,
            });
        }
    }
    out
}

/// lane 選択アクション。
#[derive(Clone, Copy)]
enum LaneNav {
    /// ◄ ページを前へ（8 lane 単位）
    PagePrev,
    /// ► ページを次へ（8 lane 単位）
    PageNext,
    /// track ボタンによる直接選択（0-7 = 現ページ内の slot index）。
    Direct(usize),
}

impl LaneNav {
    /// ログ表示用グリフ。
    fn glyph(self) -> &'static str {
        match self {
            LaneNav::PagePrev => "◄",
            LaneNav::PageNext => "►",
            LaneNav::Direct(_) => "●",
        }
    }
}

/// ROTO button index → LaneNav の解決。
///
/// MIX モードの track button 0-7 (CC20-27) = LCD 直下の物理ボタン → 現ページ内 Direct select。
/// transport ◄/► (CC36/37) = ページ送り（8 lane を超えるリストの切替）。
fn roto_lane_nav(index: u8) -> Option<LaneNav> {
    match index {
        0..=7 => Some(LaneNav::Direct(index as usize)),
        16 => Some(LaneNav::PagePrev),
        17 => Some(LaneNav::PageNext),
        _ => None,
    }
}

/// cross-project ROTO control: TheWorld (32000) から全 project の Lane を集約し、
/// 8 slot LCD + paging で選択する（Unison-native）。
///
/// 設計: async control loop。`block_on` は最外 1 回のみ。midir(sync) → tokio mpsc bridge。
/// `select!` で MIDI 入力 / `list_all_lanes` poll を同時に await。
/// - track button 0-7 = 現ページ内の lane を選択 → 対象 SP に `switch_lane` を直接送る
/// - transport ◄/► (CC36/37) = ページ送り（8 lane を超えるリストの切替、view のみ）
///
/// switch_lane は SP scope のまま（各 project の active lane は各 SP が保持）。ROTO は対象
/// lane の SP port へ QUIC で直接届ける（per-port channel を lazy cache）。TheWorld に
/// relay を足さない最小設計。
fn execute_roto_control(port: String, world_port: u16, secs: u64) -> Result<()> {
    use crate::device_input::{ControlEvent, DeviceInput, roto::RotoInput};
    use crate::device_profile::{DeviceProfile, Rgb, roto::RotoProfile};
    use std::collections::HashMap;
    use std::time::Duration;

    let (_conn_in, mut midi_rx, mut conn_out, port_name) = roto_open_async(&port)?;

    // handshake（MIDI out、sync）— demo/anim/watch と同じ DAW_START シーケンス
    let mut profile = RotoProfile::default();
    for msg in profile.handshake() {
        conn_out.send(&msg)?;
    }
    println!(
        "DAW_START 送信（{}）→ TheWorld :{} に接続中（track button で選択 / ◄► でページ送り）",
        port_name, world_port
    );

    // 現ページの 8 slot を `(label, is_active)` に展開する（projection 入力）。
    // selected と一致する lane を active 表示。
    fn page_slots(lanes: &[RotoLane], page: usize, selected: Option<&str>) -> Vec<(String, bool)> {
        let start = page * 8;
        lanes
            .iter()
            .skip(start)
            .take(8)
            .map(|l| (l.label.clone(), Some(l.key.as_str()) == selected))
            .collect()
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let world = connect_quic_local(world_port).await?;
        let world_ch = world
            .open_channel("world-process")
            .await
            .map_err(|e| anyhow::anyhow!("open world-process channel: {}", e))?;

        // switch_lane 送信用の per-SP channel cache（port → (client, process channel)）。
        // client も保持しないと接続が drop されるため tuple で抱える。
        let mut sp_clients: HashMap<u16, (unison::ProtocolClient, unison::network::UnisonChannel)> =
            HashMap::new();

        let mut input = RotoInput::default();
        let mut lanes: Vec<RotoLane> = Vec::new();
        let mut page = 0usize; // 表示ページ（1 ページ = 8 lane）
        let mut selected: Option<String> = None; // 選択中 lane key "{port}:{token}"
        let mut activated = false;
        let mut lcd_projected = false;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        // 2 秒間隔で TheWorld の全 lane を poll（最初の tick は即時 = 初期取得）
        let mut poll = tokio::time::interval(Duration::from_secs(2));

        // active lane の色（シアン）/ 非 active（暗い青灰）
        let color_active = Rgb::new(0, 200, 255);
        let color_inactive = Rgb::new(60, 80, 120);

        loop {
            tokio::select! {
                // biased: MIDI を最優先に poll（keepalive 応答遅延 → ROTO 切断を防ぐ）。
                biased;
                // MIDI 入力（midir callback → tokio mpsc）
                midi = midi_rx.recv() => {
                    let Some(bytes) = midi else { break; }; // sender drop = 終了
                    // realtime 1 byte system message は無視
                    if bytes.len() == 1 && bytes[0] >= 0xF8 {
                        continue;
                    }
                    // keepalive SysEx は自動応答（接続維持）
                    if roto_autorespond(&bytes, &mut conn_out)? {
                        continue;
                    }
                    // 最初の SysEx で activated フラグを立てる（lanes 到着済なら即 projection）
                    if !activated && bytes.first() == Some(&0xF0) {
                        activated = true;
                        if !lanes.is_empty() && !lcd_projected {
                            let slots = page_slots(&lanes, page, selected.as_deref());
                            let msgs = roto_project_slots_full(&mut profile, &slots, color_active, color_inactive);
                            for m in &msgs {
                                conn_out.send(m)?;
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            lcd_projected = true;
                        }
                        println!("接続成立 — {} lanes（track button で選択 / ◄► でページ送り）", lanes.len());
                        continue;
                    }
                    // channel message を ControlEvent に → binding 表で nav 解決
                    let Some(event) = input.parse(&bytes) else { continue; };
                    let ControlEvent::Button { index, pressed: true } = event else {
                        continue;
                    };
                    let Some(nav) = roto_lane_nav(index) else {
                        continue;
                    };
                    if lanes.is_empty() {
                        eprintln!("lane list 未取得（TheWorld :{} に SP 未登録?）", world_port);
                        continue;
                    }
                    let pages = lanes.len().div_ceil(8);
                    match nav {
                        // ページ送り（view のみ、switch_lane は送らない）
                        LaneNav::PagePrev | LaneNav::PageNext => {
                            page = match nav {
                                LaneNav::PagePrev => (page + pages - 1) % pages,
                                _ => (page + 1) % pages,
                            };
                            println!("{} page {}/{}", nav.glyph(), page + 1, pages);
                            if lcd_projected {
                                let slots = page_slots(&lanes, page, selected.as_deref());
                                let msgs = roto_project_slots(&mut profile, &slots, color_active, color_inactive);
                                for m in &msgs {
                                    conn_out.send(m)?;
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                            }
                        }
                        // 現ページ内の lane を選択 → 対象 SP へ switch_lane
                        LaneNav::Direct(slot) => {
                            let Some(lane) = lanes.get(page * 8 + slot).cloned() else {
                                continue; // 空 slot
                            };
                            selected = Some(lane.key.clone());
                            println!("● select '{}' (port {})", lane.label, lane.port);

                            // LCD ハイライト更新
                            if lcd_projected {
                                let slots = page_slots(&lanes, page, selected.as_deref());
                                let msgs = roto_project_slots(&mut profile, &slots, color_active, color_inactive);
                                for m in &msgs {
                                    conn_out.send(m)?;
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                            }

                            // 対象 SP に switch_lane を送る（per-port channel を lazy 接続）
                            if let std::collections::hash_map::Entry::Vacant(slot) = sp_clients.entry(lane.port) {
                                match connect_quic_local(lane.port).await {
                                    Ok(c) => match c.open_channel("process").await {
                                        Ok(ch) => { slot.insert((c, ch)); }
                                        Err(e) => eprintln!("process channel open 失敗 (port {}): {}", lane.port, e),
                                    },
                                    Err(e) => eprintln!("SP :{} 接続失敗: {}", lane.port, e),
                                }
                            }
                            let payload = serde_json::json!({ "type": "switch_lane", "lane": lane.token });
                            let res = match sp_clients.get(&lane.port) {
                                Some((_c, ch)) => Some(
                                    ch.request::<serde_json::Value, serde_json::Value>("switch_lane", &payload).await,
                                ),
                                None => None,
                            };
                            if let Some(Err(e)) = res {
                                eprintln!("switch_lane 失敗 (port {}): {} — cache drop", lane.port, e);
                                sp_clients.remove(&lane.port); // 次回再接続
                            }
                        }
                    }
                }
                // TheWorld の全 lane を poll → lanes 再構築（project / lane 増減を live 反映）
                _ = poll.tick() => {
                    let resp = world_ch
                        .request::<serde_json::Value, serde_json::Value>("list_all_lanes", &serde_json::json!({}))
                        .await;
                    match resp {
                        Ok(v) => {
                            let next = parse_world_lanes(&v);
                            if next != lanes {
                                lanes = next;
                                // page を範囲内に clamp
                                let pages = lanes.len().div_ceil(8).max(1);
                                page = page.min(pages - 1);
                                if activated {
                                    let slots = page_slots(&lanes, page, selected.as_deref());
                                    let msgs = roto_project_slots_full(&mut profile, &slots, color_active, color_inactive);
                                    for m in &msgs {
                                        conn_out.send(m)?;
                                        std::thread::sleep(Duration::from_millis(1));
                                    }
                                    lcd_projected = true;
                                    println!("LCD 更新: {} lanes ({} pages)", lanes.len(), pages);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("list_all_lanes 失敗（TheWorld 停止?）: {} — 終了", e);
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    println!("roto control 終了（{} 秒経過）", secs);
                    break;
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// 8 slot を ROTO LCD に projection する。`slots[i] = (label, is_active)`。
/// active slot は別色でハイライト。不足分（< 8）は空欄。
///
/// 最適化: `project_track` は 1 call ごとに全 track の枠付きバッチを返すため、
/// shadow 更新は全 slot で行うが **送信は最後の 1 バッチのみ**（完全な state を含む）。
/// `learn_parameter` は初回のみ必要（knob activation 用）。
fn roto_project_slots(
    profile: &mut crate::device_profile::roto::RotoProfile,
    slots: &[(String, bool)],
    color_active: crate::device_profile::Rgb,
    color_inactive: crate::device_profile::Rgb,
) -> Vec<Vec<u8>> {
    use crate::device_profile::DeviceProfile;

    // 全 slot の shadow を更新し、最後の batch だけ保持する
    let mut last_batch = Vec::new();
    for i in 0..8u8 {
        let (name, color) = if let Some((label, active)) = slots.get(i as usize) {
            let c = if *active {
                color_active
            } else {
                color_inactive
            };
            (label.as_str(), c)
        } else {
            ("", crate::device_profile::Rgb::new(0, 0, 0))
        };
        last_batch = profile.project_track(i, name, color, false);
    }
    last_batch
}

/// 初回接続時の完全 projection（track 表示 + knob activation）。
/// `learn_parameter` は knob を active にするために初回のみ必要。
fn roto_project_slots_full(
    profile: &mut crate::device_profile::roto::RotoProfile,
    slots: &[(String, bool)],
    color_active: crate::device_profile::Rgb,
    color_inactive: crate::device_profile::Rgb,
) -> Vec<Vec<u8>> {
    use crate::device_profile::{DeviceProfile, ParamSpec};

    let mut msgs = roto_project_slots(profile, slots, color_active, color_inactive);
    // learn_parameter で knob を active にする（入力が来るようになる）
    for i in 0..8u8 {
        let label = slots.get(i as usize).map(|(l, _)| l.as_str()).unwrap_or("");
        let spec = ParamSpec::continuous(label, 0.5);
        msgs.extend(profile.learn_parameter(i, &spec));
    }
    msgs
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

#[cfg(test)]
mod tests {
    use super::*;

    /// LCD ラベルは project 優先 + lane で 13 文字以内に収まる。
    #[test]
    fn compact_label_fits_lcd() {
        // 長い project 名 → 8 文字に truncate、token → 4 文字
        let label = compact_lane_label("vantage-point", "conductor");
        assert_eq!(label, "vantage-:cond");
        assert!(label.chars().count() <= 13);
        // 短い名前はそのまま
        assert_eq!(compact_lane_label("vp", "alpha"), "vp:alph");
    }

    /// list_all_lanes 応答 → flat な RotoLane 列。順序は **server 順をそのまま保つ**
    /// （client は再ソートしない = sidebar / project_order と一致させる契約）。
    #[test]
    fn parse_world_lanes_preserves_server_order() {
        let v = serde_json::json!({
            "projects": [
                // server が project_order で並べた前提。client は触らず保つ。
                {
                    "project_name": "zeta-proj",
                    "port": 33001,
                    "lanes": [
                        { "kind": "conductor" },
                        { "kind": "performer", "address": { "name": "beta" } },
                        { "kind": "performer", "address": { "name": "alpha" } },
                    ]
                },
                {
                    "project_name": "aaa-proj",
                    "port": 33000,
                    "lanes": [ { "kind": "conductor" } ]
                },
            ]
        });
        let lanes = parse_world_lanes(&v);
        // server 順そのまま: zeta(cond, beta, alpha) → aaa(cond)
        let keys: Vec<&str> = lanes.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "33001:conductor",
                "33001:beta",
                "33001:alpha",
                "33000:conductor",
            ]
        );
        // port が switch_lane 送信先として保持される
        assert_eq!(lanes[0].port, 33001);
        assert_eq!(lanes[3].port, 33000);
        assert_eq!(lanes[1].token, "beta");
    }

    /// projects 不在 / 空でも panic せず空 Vec。
    #[test]
    fn parse_world_lanes_empty() {
        assert!(parse_world_lanes(&serde_json::json!({})).is_empty());
        assert!(parse_world_lanes(&serde_json::json!({ "projects": [] })).is_empty());
    }
}
