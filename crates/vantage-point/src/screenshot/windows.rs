//! Windows backend stub — 将来 `Windows.Graphics.Capture` API or BitBlt + GDI で実装予定。
//!
//! 現状: 空 module (caller は `unimplemented!` 相当の挙動を想定しない)。
//! `screenshot/mod.rs` の `default_backend()` が cfg で OS 分岐するため、 macOS dogfooding 中は
//! このファイルの内容は使われない。 ただし `cargo fmt --all` が cfg を見ずに全 mod を traverse
//! するため、 stub 用にファイル自体は存在させる必要がある。
