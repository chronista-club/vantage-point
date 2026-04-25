//! KDL 1-line ログ formatter (VP-100 follow-up)
//!
//! `tracing-subscriber` のデフォルト ANSI formatter は色付き複数行も出すので、
//! 機械処理 + grep に向かない。KDL (Konfig Document Language) で 1 log = 1 node
//! = 1 line にすることで:
//!
//! - **grep / awk しやすい** — 各行が完結したレコード
//! - **構造化** — KDL の `key=value` プロパティで field を表現
//! - **読みやすい** — JSON より人間にやさしい syntax
//! - **将来 KDL parser で集計** — `unison-kdl` 等で読み込んで分析可能
//!
//! ## 出力例
//!
//! ```text
//! info ts="2026-04-25T12:34:56.789Z" target="vp_app::app" "vp-app 起動 (Creo UI mint-dark)"
//! warn ts="2026-04-25T12:34:57.012Z" target="vp_app::client" "TheWorld fetch 失敗 (daemon 未起動?): connection refused"
//! info ts="2026-04-25T12:34:58.123Z" target="vp_app::app" project_count=3 "daemon online 復帰検知"
//! ```
//!
//! - 第一トークン = log level (= KDL node 名)
//! - `ts="..."` = ISO 8601 timestamp プロパティ
//! - `target="..."` = tracing target プロパティ
//! - 任意の `key=value` = tracing structured field
//! - 末尾 `"..."` = log message (positional arg)

use std::fmt;

use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::registry::LookupSpan;

/// KDL 1-line FormatEvent
pub struct KdlFormatter;

impl<S, N> FormatEvent<S, N> for KdlFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let level = match *meta.level() {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            tracing::Level::DEBUG => "debug",
            tracing::Level::TRACE => "trace",
        };
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        write!(
            writer,
            "{} ts={} target={}",
            level,
            kdl_string(&ts),
            kdl_string(meta.target())
        )?;

        // tracing field を visit して KDL property / 末尾 message に振り分ける
        let mut visitor = KdlVisitor {
            writer: &mut writer,
            message: None,
            error: None,
        };
        event.record(&mut visitor);
        if let Some(e) = visitor.error {
            return Err(e);
        }
        if let Some(msg) = visitor.message {
            write!(writer, " {}", kdl_string(&msg))?;
        }
        writeln!(writer)
    }
}

/// tracing field の収集 + KDL 出力 (message は末尾の positional arg として保留)
struct KdlVisitor<'a, 'w> {
    writer: &'a mut Writer<'w>,
    message: Option<String>,
    error: Option<fmt::Error>,
}

impl tracing::field::Visit for KdlVisitor<'_, '_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if self.error.is_some() {
            return;
        }
        if field.name() == "message" {
            self.message = Some(value.to_string());
            return;
        }
        if let Err(e) = write!(self.writer, " {}={}", field.name(), kdl_string(value)) {
            self.error = Some(e);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if self.error.is_some() {
            return;
        }
        let s = format!("{:?}", value);
        if field.name() == "message" {
            self.message = Some(s);
            return;
        }
        if let Err(e) = write!(self.writer, " {}={}", field.name(), kdl_string(&s)) {
            self.error = Some(e);
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = write!(self.writer, " {}={}", field.name(), value) {
            self.error = Some(e);
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = write!(self.writer, " {}={}", field.name(), value) {
            self.error = Some(e);
        }
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = write!(self.writer, " {}={}", field.name(), value) {
            self.error = Some(e);
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = write!(self.writer, " {}={}", field.name(), value) {
            self.error = Some(e);
        }
    }
}

/// KDL string literal にエスケープ。改行・タブ・制御文字も `\u{HH}` で escape。
fn kdl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!(r"\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_quotes_backslash_newline() {
        assert_eq!(kdl_string(r#"he said "hi""#), r#""he said \"hi\"""#);
        assert_eq!(kdl_string(r"path\to\file"), r#""path\\to\\file""#);
        assert_eq!(kdl_string("line1\nline2"), r#""line1\nline2""#);
        assert_eq!(kdl_string("col\tval"), r#""col\tval""#);
    }

    #[test]
    fn escape_control_char() {
        assert_eq!(kdl_string("\x01"), r#""\u{1}""#);
    }

    #[test]
    fn plain_string() {
        assert_eq!(kdl_string("hello world"), r#""hello world""#);
    }
}
