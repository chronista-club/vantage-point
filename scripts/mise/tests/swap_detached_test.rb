# task: mem_1Cg5ZMy8oPgcvmM1oq8X5W
require 'minitest/autorun'
require 'tmpdir'
require 'json'
lib = File.expand_path('../swap_detached.rb', __dir__)
require lib if File.file?(lib)

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
      assert_raises(RuntimeError) { SwapDetached.submit(root: Dir.pwd, env: env, state: dir, launch: ->(_) { flunk '二重登録' }) }
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
    assert_raises(Errno::ENOENT) { @swap.restart_daemon_macos }
  end
end
