//! midistage-profiles — MIDI コントロールサーフェスのランタイム制御プロトコル（純粋計算）
//!
//! VP（DeviceRegistry/Device I/O）と bikeboy 等、複数アプリから共有される MIDI ライブラリ。
//! 責務は **data と calculations のみ**で、I/O（ポート接続・polling・送出）は含まない —
//! それは各アプリ側（VP は DeviceRegistry、bikeboy は cortex-midi）の仕事。
//!
//! - [`device_profile`] — 出力方向: アプリ state → 機材 byte 列（LED/LCD projection）
//! - [`device_input`] — 入力方向: 機材 raw byte → 論理 [`device_input::ControlEvent`]
//! - [`roto_palette`] — ROTO-CONTROL の色パレット量子化
//!
//! ## midistage ファミリーでの位置づけ
//!
//! midistage リポジトリ（KORG MIDI 2.0 configurator）とは相補関係:
//! - `midistage-core`（midistage repo）= transport 基盤（CoreMIDI FFI / UMP / KORG SysEx）
//! - `midistage-profiles`（本 crate）= ランタイム制御の device protocol（ROTO / X-Touch / LPD8 mk2）
//!
//! 現在は VP workspace に仮住まい（DeviceRegistry/Device I/O の開発 loop を保つため）。
//! converge 後に midistage リポジトリへの移住を検討する。
//! スコープは MIDI / コントロールサーフェス系に限定する（汎用置き場にしない）。
//! 経緯: vantage-point 本体 crate からの切り出し（2026-07-15 mako 決定、
//! bikeboy design/03-midi-focus-model.md も参照）。

pub mod device_input;
pub mod device_profile;
pub mod roto_palette;
