//! midistage-core — MIDI コントロールサーフェスのプロトコルと純粋計算
//!
//! VP（Bastet/Justice）と bikeboy 等、複数アプリから共有される MIDI ライブラリの集積先。
//! 責務は **data と calculations のみ**で、I/O（ポート接続・polling・送出）は含まない —
//! それは各アプリ側（VP は Bastet、bikeboy は cortex-midi）の仕事。
//!
//! - [`device_profile`] — 出力方向: アプリ state → 機材 byte 列（LED/LCD projection）
//! - [`device_input`] — 入力方向: 機材 raw byte → 論理 [`device_input::ControlEvent`]
//! - [`roto_palette`] — ROTO-CONTROL の色パレット量子化
//!
//! スコープは MIDI / コントロールサーフェス系に限定する（汎用置き場にしない）。
//! 経緯: vantage-point 本体 crate からの切り出し（2026-07-15 mako 決定、
//! bikeboy design/03-midi-focus-model.md も参照）。

pub mod device_input;
pub mod device_profile;
pub mod roto_palette;
