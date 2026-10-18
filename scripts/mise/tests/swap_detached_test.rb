# task: mem_1Cg5ZMy8oPgcvmM1oq8X5W
require 'minitest/autorun'
require 'tmpdir'
require 'json'
require 'rbconfig'
# Scoped replacements keep every OS mutation inside the test sandbox.
module ScopedStub
  def stub(name, replacement)
    original = method(name)
    singleton_class.define_method(name) { |*args, **kwargs| replacement.respond_to?(:call) ? replacement.call(*args, **kwargs) : replacement }
    yield
  ensure
    singleton_class.define_method(name, original)
  end
end

lib = File.expand_path('../swap_detached.rb', __dir__)
require lib if File.file?(lib)
[SwapDetached, File, FileUtils].each { |target| target.extend(ScopedStub) }

class SwapDetachedTest < Minitest::Test
  def test_submit_is_one_shot_sanitized_and_locked
    assert defined?(SwapDetached), 'detached 受付が必要'
    Dir.mktmpdir do |dir|
      submitted = []
      env = {'HOME'=>Dir.home, 'VP_LANE'=>'swap-detached', 'VP_REPO'=>'vantage-point', 'VP_SESSION_KEY'=>'42', 'VP_SWAP_RESTART_DAEMON'=>'1'}
      job = SwapDetached.submit(root: Dir.pwd, env: env, state: dir, launch: ->(plist) { submitted << File.read(plist); true })
      assert_equal 1, submitted.size
      assert_includes submitted.first, '<key>RunAtLoad</key><true/>'
      assert_includes submitted.first, '<key>KeepAlive</key><false/>'
      assert_includes submitted.first, '<key>AbandonProcessGroup</key><true/>'
      refute_includes submitted.first, 'VP_SESSION_KEY'
      assert_equal 'agent@vantage-point/swap-detached', job.fetch('to')
      assert job.fetch('log').start_with?(dir)
      assert_raises(RuntimeError) { SwapDetached.submit(root: Dir.pwd, env: env, state: dir, launch: ->(_) { flunk '二重登録' }, status: ->(_) { :running }) }
    end
  end

  def test_failed_registration_releases_lock
    assert defined?(SwapDetached), 'detached 受付が必要'
    Dir.mktmpdir do |dir|
      assert_raises(RuntimeError) { SwapDetached.submit(root: Dir.pwd, env: {}, state: dir, launch: ->(_) { false }) }
      refute File.exist?(File.join(dir, 'active.lock'))
    end
  end

  def test_warning_is_sent_to_all_lanes_before_waiting
    assert defined?(SwapDetached), '予告の配信が必要'
    events = []
    job = {'to'=>'agent@vp/a'}
    SwapDetached.announce(job, addresses: ['agent@vp/a', 'agent@other', 'agent@vp/a'], send: ->(to, body) { events << [to, body] }, wait: ->(seconds) { events << seconds })
    assert_equal ['agent@vp/a', 'agent@other'], events[0..1].map(&:first)
    assert_includes events[0][1], '30 秒後'
    assert_equal 30, events.last
  end
end

# task 定義だけを隔離して読む。末尾の OS dispatch は実行しない。
class SwapReadinessTest < Minitest::Test
  def setup
    ENV['MISE_PROJECT_ROOT'] = File.expand_path('../../..', __dir__)
    task = File.read(File.join(ENV['MISE_PROJECT_ROOT'], '.mise/tasks/app/swap'))
    dispatch = "VP.windows? ? swap_windows(RESTART_DAEMON) : swap_macos(RESTART_DAEMON)\n"
    raise 'task の入口が変更されています。安全な loader を更新してください' unless task.end_with?(dispatch)
    definitions = Module.new
    definitions.module_eval(task.delete_suffix(dispatch), 'swap-task-definitions')
    # system 操作は一切許さない。予告が外れた場合も本物を再起動せず検出する。
    vp = Module.new do
      def self.log(*) = nil
      def self.icon(*) = ''
      def self.launch_agent_loaded? = true
      def self.run(*) = raise('restart attempted')
    end
    vp.const_set(:LAUNCH_AGENT_LABEL, 'test-only')
    definitions.const_set(:VP, vp)
    net = Module.new
    http = Class.new do
      def self.start(*) = raise(Errno::ECONNREFUSED)
    end
    net.const_set(:HTTP, http)
    definitions.const_set(:Net, net)
    @swap = Object.new.extend(definitions)
    @previous_job = ENV['VP_SWAP_JOB']
  end

  def teardown
    ENV['VP_SWAP_JOB'] = @previous_job
  end

  def test_detached_readiness_timeout_fails_instead_of_reporting_success
    ENV['VP_SWAP_JOB'] = '/test/job.json'
    @swap.define_singleton_method(:app_log_path) { '/nonexistent/swap-test.log' }
    assert_raises(RuntimeError) { @swap.wait_app_ready(0, timeout: -1) }
  end

  def test_detached_daemon_timeout_fails
    ENV['VP_SWAP_JOB'] = '/test/job.json'
    assert_raises(RuntimeError) { @swap.wait_daemon_ready(timeout: -1) }
  end

  def test_direct_swap_keeps_existing_warning_behavior
    ENV.delete('VP_SWAP_JOB')
    @swap.define_singleton_method(:app_log_path) { '/nonexistent/swap-test.log' }
    assert_equal false, @swap.wait_app_ready(0, timeout: -1)
  end

  def test_notice_failure_stops_before_any_restart
    ENV['VP_SWAP_JOB'] = '/nonexistent/swap-test.json'
    events = sandbox_swap
    File.stub(:directory?, true) do
      FileUtils.stub(:rm_rf, ->(_) { events << :replace }) do
        assert_raises(Errno::ENOENT) { @swap.swap_macos(true) }
      end
    end
    assert_equal [:build], events
  end
end

class SwapDetachedTest
  def test_wire_uses_command_delivery
    args = nil
    SwapDetached.stub(:capture, ->(*a) { args = a }) { SwapDetached.send_wire('agent@vp', 'test') }
    assert_equal 'command', args[args.index('--category') + 1]
  end

  def test_plist_and_tmpdir
    Dir.mktmpdir do |dir|
      job = SwapDetached.submit(root: Dir.pwd, state: dir, env: {'TMPDIR'=>'/tmp/a b'}, launch: ->(_) { true })
      assert system('/usr/bin/plutil', '-lint', job['plist'], out: File::NULL)
      assert_includes File.read(job['plist']), 'TMPDIR=/tmp/a b'
      assert_equal job['label'], JSON.parse(File.read(File.join(job['lock'], 'owner.json')))['label']
    end
  end

  def test_stale_owner_reclaimed_but_running_and_unknown_rejected
    [:absent, :exited, :running, :unknown].each do |state|
      Dir.mktmpdir do |dir|
        old = SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, launch: ->(_) { true })
        calls = []
        submit = -> { SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, launch: ->(_) { true },
          status: ->(label) { assert_equal old['label'], label; state },
          unload: ->(label) { calls << label; true }) }
        if [:absent, :exited].include?(state)
          refute_equal old['label'], submit.call['label']
          assert_equal(state == :exited ? [old['label']] : [], calls)
        else
          assert_raises(RuntimeError, &submit)
        end
      end
    end
  end

  def test_worker_bootout_even_if_result_write_fails
    Dir.mktmpdir do |dir|
      job = SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, launch: ->(_) { true }, command: ['false'])
      calls = []
      previous = ENV.to_h
      SwapDetached.stub(:capture, 'test') do
        SwapDetached.stub(:system, ->(*args) { calls << args; false }) do
          File.stub(:write, ->(*) { raise IOError, 'disk full' }) do
            SwapDetached.work(job['config'])
          end
        end
      end
      refute File.exist?(job['lock'])
      assert_equal ['/bin/launchctl', 'bootout', "gui/#{Process.uid}/#{job['label']}"], calls.last
    ensure
      ENV.replace(previous) if previous
    end
  end

  def test_nonselected_running_repo_and_empty_list
    SwapDetached.stub(:capture, ->(*args, **_) { args[1] == 'repos' ? "○ other [running]  /tmp/other\n" : '{"lanes":[{"address":{"repo":"other","name":"root"}}]}' }) do
      assert_equal ['agent@other'], SwapDetached.addresses
    end
    SwapDetached.stub(:capture, "○ other  /tmp/other\n") { assert_empty SwapDetached.addresses }
    SwapDetached.stub(:capture, "(登録 repo なし)\n") { assert_empty SwapDetached.addresses }
    SwapDetached.stub(:capture, 'unrecognized output') { assert_raises(RuntimeError) { SwapDetached.addresses } }
    SwapDetached.stub(:capture, 'broken [running]') { assert_raises(RuntimeError) { SwapDetached.addresses } }
  end
end

class SwapReadinessTest
  def sandbox_swap
    events = []
    vp = @swap.method(:swap_macos).owner.const_get(:VP)
    vp.define_singleton_method(:sh) { |cmd| events << (cmd.include?('release:mac') ? :build : :shell) }
    vp.define_singleton_method(:vp_bin) { '/fake/vp' }
    vp.define_singleton_method(:try) { |*_| events << :stop }
    vp.define_singleton_method(:run) { |*_| events << :start }
    @swap.define_singleton_method(:sleep) { |_| }
    @swap.define_singleton_method(:app_log_pos) { 0 }
    @swap.define_singleton_method(:wait_app_ready) { |_| }
    @swap.define_singleton_method(:swap_epilogue) { |_| }
    @swap.define_singleton_method(:brew_restore_hint) { |_| }
    # File/OS mutations must be stubbed by the calling test.
    events
  end

  def test_notice_then_stop_and_failure_restores_app
    assert_recovery_after_failure(RuntimeError.new('readiness failed'))
  end

  def test_command_exit_restores_app
    assert_recovery_after_failure(SystemExit.new(7, 'command failed'))
  end

  def assert_recovery_after_failure(failure)
    events = sandbox_swap
    @swap.define_singleton_method(:restart_daemon_macos) { events << :restart; raise failure }
    File.stub(:directory?, true) do
      File.stub(:executable?, true) do
        FileUtils.stub(:rm_rf, ->(_) { events << :replace }) do
          SwapDetached.stub(:before_restart, -> { events << :notice }) do
            error = assert_raises(failure.class) { @swap.swap_macos(true) }
            assert_same failure, error
          end
        end
      end
    end
    assert_operator events.index(:build), :<, events.index(:notice)
    assert_operator events.index(:notice), :<, events.index(:stop)
    assert_equal :start, events.last
  end
end

class SwapDetachedTest
  def test_launchd_state_classification
    cases = [
      ["state = not running\nlast exit code = 7\n", '', 0, :exited],
      ["state = not running\nlast terminating signal = Killed: 9\n", '', 0, :exited],
      ["state = running\npid = 123\n", '', 0, :running],
      ["state = spawn scheduled\n", '', 0, :unknown],
      ['', 'Could not find service "test" in domain for user gui: 501', 113, :absent],
      ['', 'permission denied', 1, :unknown]
    ]
    Open3.extend(ScopedStub)
    cases.each do |out, err, code, expected|
      status = Struct.new(:exitstatus) { def success? = exitstatus == 0 }.new(code)
      Open3.stub(:capture3, [out, err, status]) { assert_equal expected, SwapDetached.job_status('test') }
    end
  end

  def test_worker_success_and_failure_results_and_cleanup
    [true, false].each do |ok|
      Dir.mktmpdir do |dir|
        job = SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, launch: ->(_) { true }, command: ['dummy'])
        calls = []
        previous = ENV.to_h
        SwapDetached.stub(:capture, 'test') do
          SwapDetached.stub(:system, ->(*args) { calls << args; ok }) { SwapDetached.work(job['config']) }
        end
        assert_equal ok, JSON.parse(File.read("#{job['config']}.result"))['success']
        refute File.exist?(job['lock'])
        assert_equal 'bootout', calls.last[1]
      ensure
        ENV.replace(previous) if previous
      end
    end
  end
end

class SwapDetachedTest
  %w[cli open recovery_cli recovery_open].each do |route|
    define_method("test_long_lived_gui_does_not_block_next_submit_#{route}") do
      Dir.mktmpdir do |dir|
        # 実 worker と swap task を使い、破壊的な OS 操作だけを置き換える。
        # GUI 相当の子は `vp app start` 同様に fork し、job 終了後も生存する。
        probe = File.join(dir, 'swap_probe.rb')
        pid_path = File.join(dir, 'gui.pid')
        File.write(probe, <<~'RUBY')
          require 'rbconfig'
          root, route, pid_path = ARGV
          ENV['MISE_PROJECT_ROOT'] = root
          task = File.read(File.join(root, '.mise/tasks/app/swap'))
          dispatch = "VP.windows? ? swap_windows(RESTART_DAEMON) : swap_macos(RESTART_DAEMON)\n"
          raise 'unsafe task loader' unless task.end_with?(dispatch)
          definitions = Module.new
          definitions.module_eval(task.delete_suffix(dispatch), 'swap-task-definitions')
          swap = Object.new.extend(definitions)
          gui = 'pid = fork { sleep 60 }; File.write(ARGV.fetch(0), pid)'
          run = VP.method(:run)
          vp = Module.new
          vp.define_singleton_method(:run) do |*args, **opts|
            raise "unexpected command: #{args}" unless args == ['/Applications/VantagePoint.app/Contents/MacOS/vp', 'app', 'start'] || args == ['open', '/Applications/VantagePoint.app']
            run.call(RbConfig.ruby, '-e', gui, pid_path, **opts)
          end
          vp.define_singleton_method(:sh) do |cmd|
            if cmd.start_with?('open ')
              run.call(RbConfig.ruby, '-e', gui, pid_path)
            elsif cmd.start_with?('codesign ') && route.start_with?('recovery_')
              raise 'simulated post-stop failure'
            elsif !['VP_RELEASE_DRY=1 ', 'pkill ', 'ditto ', 'codesign '].any? { |prefix| cmd.start_with?(prefix) }
              raise "unexpected shell command: #{cmd}"
            end
          end
          vp.define_singleton_method(:vp_bin) { nil }
          vp.define_singleton_method(:log) { |*_| }
          vp.define_singleton_method(:icon) { |_| '' }
          definitions.const_set(:VP, vp)
          File.define_singleton_method(:directory?) { |_| true }
          File.define_singleton_method(:executable?) { |_| route.end_with?('cli') }
          FileUtils.define_singleton_method(:rm_rf) { |_| }
          swap.define_singleton_method(:sleep) { |_| }
          swap.define_singleton_method(:app_log_pos) { 0 }
          swap.define_singleton_method(:wait_app_ready) { |_| }
          swap.define_singleton_method(:brew_restore_hint) { |_| }
          swap.swap_macos(false)
        RUBY
        job = SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, launch: ->(_) { true },
          command: [RbConfig.ruby, probe, Dir.pwd, route, pid_path])
        worker = fork do
          SwapDetached.stub(:bootout, true) { SwapDetached.work(job['config']) }
          exit! 0
        end
        Process.wait(worker)
        assert_predicate $?, :success?
        assert_equal !route.start_with?('recovery_'), JSON.parse(File.read("#{job['config']}.result"))['success']
        gui_pid = Integer(File.read(pid_path))
        Process.kill(0, gui_pid)
        begin
          next_job = SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, launch: ->(_) { true })
        rescue RuntimeError => e
          flunk "GUI still holds worker.lock after #{route}: #{e.message}"
        end
        refute_equal job['label'], next_job['label']
      ensure
        if File.file?(pid_path)
          begin
            Process.kill('KILL', Integer(File.read(pid_path)))
          rescue Errno::ESRCH
            # 先に終了した模擬 GUI は回収不要。
          end
        end
      end
    end
  end

  def test_orphan_command_lease_prevents_reclaim
    Dir.mktmpdir do |dir|
      SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, launch: ->(_) { true })
      File.open(File.join(dir, 'worker.lock'), File::RDWR | File::CREAT, 0o600) do |lease|
        lease.flock(File::LOCK_EX)
        assert_raises(RuntimeError) do
          SwapDetached.submit(root: Dir.pwd, state: dir, env: {}, status: ->(_) { :exited },
            unload: ->(_) { true }, launch: ->(_) { true })
        end
      end
    end
  end
end

class SwapDetachedTest
  def test_notification_and_lock_cleanup_failure_still_bootout
    Dir.mktmpdir do |dir|
      job = SwapDetached.submit(root: Dir.pwd, state: dir, env: {'VP_REPO'=>'test'}, launch: ->(_) { true }, command: ['dummy'])
      previous = ENV.to_h
      calls = []
      SwapDetached.stub(:capture, 'test') do
        SwapDetached.stub(:system, ->(*args) { calls << args; true }) do
          SwapDetached.stub(:send_wire, ->(*) { raise 'notification failed' }) do
            FileUtils.stub(:remove_entry, ->(*) { raise 'cleanup failed' }) { SwapDetached.work(job['config']) }
          end
        end
      end
      assert JSON.parse(File.read("#{job['config']}.result"))['success']
      assert_equal 'bootout', calls.last[1]
      assert File.exist?(job['lock']), 'failed cleanup must remain reclaimable'
    ensure
      ENV.replace(previous) if previous
    end
  end
end
