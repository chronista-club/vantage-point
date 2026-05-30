# frozen_string_literal: true
#
# VP — file-based mise task の共有 helper (標準 Ruby runtime)。
#
# Gold Experience (埋め込み Ruby VM / Vp.connect DSL) とは別系統。 こちらは
# dev-machine orchestration (build / install / process spawn) を素の Ruby で書くための薄い土台。
#
# 使い方 (各 task file 冒頭):
#   require File.join(ENV.fetch("MISE_PROJECT_ROOT"), "scripts/mise/vp.rb")
#   VP.sh "cargo build"
#
# MISE_PROJECT_ROOT は mise が file task に必ず注入する env (= config_root)。 これで
# task の nesting 深さ (.mise/tasks/a/b/c) に依らず helper を絶対 path で解決でき、
# helper を tasks dir の外 (scripts/mise/) に置けるので mise の task 検出と衝突しない。
#
# 設計方針 (CLAUDE.md): data / calculations / actions を分離。
#   - calculations = 純粋関数 (path 計算のみ、 副作用なし)
#   - actions      = sh / exec / die / log (副作用あり)
module VP
  module_function

  # ── calculations (純粋) ───────────────────────────────────

  # mise 注入の project root。 nesting 深さに依らず絶対解決の基点。
  def root = ENV.fetch("MISE_PROJECT_ROOT")

  # ~/.cargo/bin/<name> の絶対 path。 codesign 済 binary の single source
  # (cp だと codesign が剥がれて macOS に kill される → install 経由必須、 CLAUDE.md feedback)。
  def cargo_bin(name) = File.join(Dir.home, ".cargo", "bin", name)

  # VP の log 出力先 (daemon:start が書き、 logs が tail する共通 dir)。
  # XDG_STATE_HOME → ~/.local/state を基底に vp/log。 VP_LOG_DIR で上書き可。
  def log_dir = ENV["VP_LOG_DIR"] || File.join(ENV["XDG_STATE_HOME"] || File.join(Dir.home, ".local", "state"), "vp", "log")

  # ── actions (副作用) ──────────────────────────────────────

  # 進捗ログ (cyan ▶)。 task の stdout を汚さないよう stderr へ。
  def log(msg) = warn("\e[36m▶\e[0m #{msg}")

  # error を stderr に出して非ゼロ終了 (bash の `echo … >&2; exit 1` 相当)。
  def die(msg, code: 1)
    warn("\e[31m✗\e[0m #{msg}")
    exit(code)
  end

  # shell command を実行し、 失敗したら die (bash の `set -e` 相当)。
  # 実行内容の透明性のため「$ cmd」を dim グレーで先に見せる。
  def sh(cmd)
    warn("\e[2m$ #{cmd}\e[0m")
    return if system(cmd)

    status = $?.exitstatus
    die("command failed (exit #{status}): #{cmd}", code: status.zero? ? 1 : status)
  end

  # 現プロセスを command で置き換える (bash の `exec` 相当)。 thin wrapper task 用
  # (例: app:stop = `exec vp app stop`)。 env で追加環境変数を渡せる。
  def exec(*cmd, env: {})
    Kernel.exec(env.transform_keys(&:to_s), *cmd)
  end

  # binary が実行可能か確認、 無ければ hint 付きで die (bash の `[ -x ] || { … exit 1; }` 相当)。
  def need_exec(path, hint:)
    File.executable?(path) or die("not found: #{path} — #{hint}")
  end
end
