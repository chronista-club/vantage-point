# frozen_string_literal: true
require 'json'
require 'fileutils'
require 'open3'
require 'cgi'
require 'securerandom'
require 'shellwords'

# launchd が所有する一回きりの swap。受付時の lane 環境は JSON の宛先にだけ畳む。
module SwapDetached
  module_function

  def submit(root:, env: ENV, state: nil, launch: nil, command: nil)
    profile = env.fetch('VP_PROFILE', '')
    raise 'invalid VP_PROFILE' unless profile.match?(/\A[a-zA-Z0-9_-]*\z/)
    state ||= File.join(env.fetch('XDG_STATE_HOME', File.join(Dir.home, '.local/state')), 'vp', 'swap')
    FileUtils.mkdir_p(state, mode: 0o700)
    lock = File.join(state, 'active.lock')
    begin
      Dir.mkdir(lock, 0o700)
    rescue Errno::EEXIST
      raise "swap は受付済みです: #{lock}（ログを確認してください）"
    end
    begin
      id = "#{Time.now.strftime('%Y%m%d-%H%M%S')}-#{SecureRandom.hex(4)}"
      label = "club.chronista.vantage-point.swap.#{id}"
      lane = env['VP_LANE']
      repo = env['VP_REPO']
      to = repo && "agent@#{repo}#{lane && !%w[lead root main conductor].include?(lane) ? "/#{lane}" : ''}"
      job = {'root'=>root, 'label'=>label, 'lock'=>lock, 'log'=>File.join(state, "#{id}.log"),
             'to'=>to, 'profile'=>profile, 'restart'=>env['VP_SWAP_RESTART_DAEMON'] == '1',
             'probe'=>!command.nil?, 'command'=>command || ['mise', 'run', 'app:swap']}
      config = File.join(state, "#{id}.json")
      job['config'] = config
      job['plist'] = File.join(state, "#{id}.plist")
      File.write(config, JSON.pretty_generate(job), mode: 'w', perm: 0o600)
      # launchd 自身に付いた VP_* も除去し、login shell で通常の開発 PATH を構成する。
      script = File.expand_path(__FILE__)
      args = ['/usr/bin/env', '-i', "HOME=#{Dir.home}", "USER=#{ENV.fetch('USER')}",
              'PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin',
              '/bin/zsh', '-lc', "exec ruby #{Shellwords.escape(script)} #{Shellwords.escape(config)}"]
      strings = args.map { |arg| "<string>#{CGI.escapeHTML(arg)}</string>" }.join
      xml = <<~XML
        <?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0"><dict>
        <key>Label</key><string>#{label}</string>
        <key>ProgramArguments</key><array>#{strings}</array>
        <key>WorkingDirectory</key><string>#{CGI.escapeHTML(root)}</string>
        <key>RunAtLoad</key><true/><key>KeepAlive</key><false/>
        <key>AbandonProcessGroup</key><true/>
        <key>StandardOutPath</key><string>#{CGI.escapeHTML(job['log'])}</string>
        <key>StandardErrorPath</key><string>#{CGI.escapeHTML(job['log'])}</string>
        </dict></plist>
      XML
      File.write(job['plist'], xml, mode: 'w', perm: 0o600)
      launch ||= ->(plist) { system('/bin/launchctl', 'bootstrap', "gui/#{Process.uid}", plist) }
      raise 'launchd 登録失敗' unless launch.call(job['plist'])
      job
    rescue Exception
      Dir.rmdir(lock)
      raise
    end
  end

  def capture(*args, **opts)
    out, err, status = Open3.capture3(*args, **opts)
    raise "#{args.first}: #{err}" unless status.success?
    out
  end

  def send_wire(to, text)
    capture('vp', 'wire', 'send', '--to', to, '--category', 'event', '--from', 'app:swap:detached', '--body', text)
  end

  # CLI の repo list は name / path を表示する。未知の形式は再起動せず失敗させる。
  def addresses
    lines = capture('vp', 'repos', 'list').lines
    paths = lines.filter_map do |line|
      next unless line.include?('[running]')
      match = line.match(/\A● .+ \[running\]  (.+)\n?\z/)
      raise "repo 一覧の形式が不明: #{line}" unless match
      match[1]
    end
    raise '稼働 repo を列挙できません' if paths.empty?
    paths.flat_map do |path|
      JSON.parse(capture('vp', 'lane', 'ls', '--detail', chdir: path)).fetch('lanes').map do |lane|
        addr = lane.fetch('address')
        name = addr.fetch('name')
        "agent@#{addr.fetch('repo')}#{%w[lead root main conductor].include?(name) ? '' : "/#{name}"}"
      end
    end.uniq
  end

  def announce(job, addresses: self.addresses, send: method(:send_wire), wait: method(:sleep))
    text = "30 秒後に daemon を再起動します（app:swap、打ったのは #{job['to'] || '外部 terminal'}）"
    addresses.uniq.each { |address| send.call(address, text) }
    wait.call(30)
  end

  def before_restart
    config = ENV['VP_SWAP_JOB']
    announce(JSON.parse(File.read(config))) if config
  end

  def work(config)
    job = JSON.parse(File.read(config))
    $stdout.sync = $stderr.sync = true
    success = false
    version = commit = 'unknown'
    begin
      # login startup で再導入された lane identity も子へ渡さない。
      ENV.keys.grep(/\AVP_/).each { |key| ENV.delete(key) }
      ENV['VP_PROFILE'] = job.fetch('profile') unless job.fetch('profile').empty?
      ENV['VP_SWAP_RESTART_DAEMON'] = job.fetch('restart') ? '1' : '0'
      ENV['VP_SWAP_JOB'] = config
      Dir.chdir(job.fetch('root'))
      commit = capture('git', 'rev-parse', 'HEAD').strip
      version = File.read('Cargo.toml')[/^version\s*=\s*"([^"]+)"/, 1] || 'unknown'
      puts "swap start version=#{version} commit=#{commit}"
      success = system(*job.fetch('command'))
      raise 'swap command failed' unless success
    rescue StandardError => e
      success = false
      warn "swap failed: #{e.message}"
    ensure
      result = "#{job['probe'] ? '[検証・swap 未実行]' : 'app:swap'} #{success ? '成功' : '失敗'} / #{success && !job['probe'] ? '反映' : '対象'} version=#{version} commit=#{commit} / log=#{job['log']}"
      puts result
      begin
        send_wire(job['to'], result) if job['to']
      rescue StandardError => e
        warn "完了通知失敗: #{e.message}"
      end
      File.write("#{config}.result", JSON.generate(success: !!success, version: version, commit: commit))
      Dir.rmdir(job.fetch('lock'))
      # bootout は自分を終了させるため、ログと結果・lock 解放を先に完了する。
      system('/bin/launchctl', 'bootout', "gui/#{Process.uid}/#{job.fetch('label')}")
    end
  end
end

SwapDetached.work(ARGV.fetch(0)) if $PROGRAM_NAME == __FILE__
