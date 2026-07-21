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
  # ── コンソール annotation の semantic icon registry ──────────────────────────
  #
  # 「意味 → glyph」を 1 箇所に集約し、 task は `VP.icon(:build)` / helper は `Icon[:ok]` の
  # ように **意味名** で呼ぶ (= console 表示の icon 選択肢を「名前」で広げる)。 glyph は Nerd Font
  # (Material Design Icons 主軸 = Phosphor に近い line-icon) の PUA codepoint で、 install 済の
  # Nerd Font (creo-tokens.css の `--typography-family-mono` primary、 例 FiraCode NF) で実描画される。
  # 全 codepoint は FiraCode Nerd Font で実在確認済 (2026-05-30)。
  #
  # Nerd Font 不在環境 (一部 CI 等) は `VP_ICONS=ascii` で ASCII に degrade。 将来 Phosphor-exact
  # 化は Phosphor を cell-fit patch して各値を Phosphor PUA に差し替えるだけ (呼び出し側は不変)。
  module Icon
    # name => [Nerd Font glyph, ASCII fallback]
    SET = {
      ok:     ["\u{F05E0}", "[ok]"],    # md-check-circle
      err:    ["\u{F0159}", "[x]"],     # md-close-circle
      warn:   ["\u{F0026}", "[!]"],     # md-alert
      info:   ["\u{F02FC}", "[i]"],     # md-information
      arrow:  ["\u{F0142}", ">"],       # md-chevron-right (progress bullet)
      build:  ["\u{F08E4}", "[build]"], # md-hammer
      rocket: ["\u{F0AB6}", "[spawn]"], # md-rocket
      bomb:   ["\u{F0691}", "[kill]"],  # md-bomb
      stop:   ["\u{F0666}", "[stop]"],  # md-stop-circle
      sync:   ["\u{F04E6}", "[~]"],     # md-sync
      note:   ["\u{F09A8}", "-"],       # md-note-text
      folder: ["\u{F07B}",  "[d]"],     # fa-folder
      test:   ["\u{F0668}", "[test]"],  # md-test-tube
    }.freeze

    # Nerd Font 不在環境向け ASCII degrade (VP_ICONS=ascii)。
    GLYPH = SET.transform_values { |(nf, asc)| ENV["VP_ICONS"] == "ascii" ? asc : nf }.freeze

    # 意味名 → glyph。 未知名は "?" に degrade (壊さない)。
    def self.[](name) = GLYPH.fetch(name, "?")
  end

  module_function

  # ── calculations (純粋) ───────────────────────────────────

  # mise 注入の project root。 nesting 深さに依らず絶対解決の基点。
  def root = ENV.fetch("MISE_PROJECT_ROOT")

  # 実行中ホストが Windows か (path / binary 拡張子の分岐に使う)。
  def windows? = Gem.win_platform?

  # ~/.cargo/bin/<name> の絶対 path。 codesign 済 binary の single source
  # (cp だと codesign が剥がれて macOS に kill される → install 経由必須、 CLAUDE.md feedback)。
  # Windows では cargo install が `<name>.exe` を置くので拡張子を補う (= `vp app` 側の
  # binary_candidates と同じ Windows 対応、 dual source drift 防止)。
  def cargo_bin(name)
    exe = windows? ? "#{name}.exe" : name
    File.join(Dir.home, ".cargo", "bin", exe)
  end

  # macOS LaunchAgent の Label（daemon/process.rs の `LAUNCH_AGENT_LABEL` と同形）。
  LAUNCH_AGENT_LABEL = "club.chronista.vantage-point.daemon"

  # 「普段使いの vp CLI」の絶対 path（実在する物を選ぶ）。 無ければ nil。
  #
  # mac の実体は **.app 同梱 CLI**（brew cask が /opt/homebrew/bin/vp から symlink する）。
  # `~/.cargo/bin/vp` は cargo install した開発者にしか無い。
  #
  # 2026-07-14 dogfood 事故: app:swap が `cargo_bin("vp")` 決め打ちで `vp app stop` /
  # `vp daemon stop` を呼んでいたため、 brew 運用の機では binary 不在 → `|| true` で
  # **サイレント失敗** → **daemon が一度も再起動されず server 変更が反映されない**、を長時間踏んだ。
  # 「実行できる物を選ぶ」を single source にして再発を防ぐ。
  def vp_bin
    candidates =
      if windows?
        [cargo_bin("vp")]
      else
        ["/Applications/VantagePoint.app/Contents/MacOS/vp", "/opt/homebrew/bin/vp", cargo_bin("vp")]
      end
    candidates.find { |p| File.executable?(p) }
  end

  # LaunchAgent が launchd に load 済みか（= `vp daemon install` 実施済み）。
  def launch_agent_loaded?
    return false if windows?

    try("launchctl", "list", LAUNCH_AGENT_LABEL, out: File::NULL, err: File::NULL)
  end

  # 走っている SP の PID 一覧。
  #
  # doc 44 P1 (fold-in): `sp_pids` / `ancestor_sp_pid` は撤去した。
  # project が World プロセス内の Arc<AppState> になり、独立した SP プロセスが
  # 存在しなくなったため、`pgrep -f 'vp sp start'` は永久に 0 件マッチだった
  # (= 呼び手が「SP は居ない」と誤認する silent no-op)。
  # 「自分が VP の lane の中にいるか」を知りたい場合は ENV["VP_LANE"] を見ること。

  # VP の log 出力先 (daemon:start が書き、 logs が tail する共通 dir)。
  # XDG_STATE_HOME → ~/.local/state を基底に vp/log。 VP_LOG_DIR で上書き可。
  def log_dir = ENV["VP_LOG_DIR"] || File.join(ENV["XDG_STATE_HOME"] || File.join(Dir.home, ".local", "state"), "vp", "log")

  # ── actions (副作用) ──────────────────────────────────────

  # semantic icon glyph を返す (task 側: `VP.log "#{VP.icon(:build)} Building…"`)。
  def icon(name) = Icon[name]

  # 進捗ログ (cyan chevron)。 task の stdout を汚さないよう stderr へ。
  def log(msg) = warn("\e[36m#{Icon[:arrow]}\e[0m #{msg}")

  # error を stderr に出して非ゼロ終了 (bash の `echo … >&2; exit 1` 相当)。
  def die(msg, code: 1)
    warn("\e[31m#{Icon[:err]}\e[0m #{msg}")
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

  # VP.sh の shell 非経由版 (引数配列をそのまま exec)。 失敗したら die。
  # Windows の system(string) は cmd.exe を噛ませるため、 `/` 区切り path・single quote・
  # `|| true` といった POSIX shell idiom が崩れる。 path や引用符を含む command は
  # 全 OS でこちらを使う (Unix でも system(*argv) は shell を介さず同挙動)。
  def run(*argv, **opts)
    warn("\e[2m$ #{argv.join(' ')}\e[0m")
    return if system(*argv, **opts)

    status = $?&.exitstatus
    die("command failed (exit #{status}): #{argv.join(' ')}", code: status.to_i.zero? ? 1 : status)
  end

  # run の「失敗を許す」版 (bash の `cmd || true` 相当)。 成否を bool で返す。
  # command 不在時に system が返す nil も false に畳む。
  def try(*argv, **opts) = system(*argv, **opts) ? true : false

  # binary が実行可能か確認、 無ければ hint 付きで die (bash の `[ -x ] || { … exit 1; }` 相当)。
  def need_exec(path, hint:)
    File.executable?(path) or die("not found: #{path} — #{hint}")
  end
end
