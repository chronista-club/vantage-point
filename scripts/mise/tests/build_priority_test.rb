# task: mem_1CgDHarsYzSdNQbvLzYqoQ
require "minitest/autorun"
require "tmpdir"
require "json"
require "rbconfig"
require_relative "../vp"

class BuildPriorityTest < Minitest::Test
  def setup
    @dir = Dir.mktmpdir("vp build priority ")
    @previous_priority = ENV["VP_BUILD_PRIORITY"]
  end

  def teardown
    ENV["VP_BUILD_PRIORITY"] = @previous_priority
    FileUtils.remove_entry(@dir)
  end

  def executable(name, executable: true)
    path = File.join(@dir, name)
    File.write(path, "")
    File.chmod(executable ? 0o755 : 0o644, path)
    path
  end

  def prefix(host_os, priority: nil)
    assert_respond_to VP, :build_prefix, "build commands need a shared priority policy"
    VP.build_prefix(host_os: host_os, priority: priority, path: @dir)
  end

  def test_macos_prefers_utility_over_nice
    policy = executable("taskpolicy")
    executable("nice")
    assert_equal [policy, "-c", "utility"], prefix("darwin27")
  end

  def test_unix_without_taskpolicy_falls_back_to_nice
    nice = executable("nice")
    assert_equal [nice, "-n", "10"], prefix("linux-gnu")
    assert_equal [nice, "-n", "10"], prefix("darwin27")
  end

  def test_linux_does_not_use_a_command_named_taskpolicy
    executable("taskpolicy")
    nice = executable("nice")
    assert_equal [nice, "-n", "10"], prefix("linux-gnu")
  end

  def test_missing_or_non_executable_commands_leave_build_usable
    executable("taskpolicy", executable: false)
    Dir.mkdir(File.join(@dir, "nice"))
    assert_empty prefix("darwin27")
    assert_empty prefix("linux-gnu")
  end

  def test_windows_and_explicit_normal_bypass_priority
    executable("taskpolicy")
    executable("nice")
    %w[mingw32 mswin64 cygwin].each { |os| assert_empty prefix(os) }
    %w[darwin27 linux-gnu].each { |os| assert_empty prefix(os, priority: "normal") }
  end

  def test_build_run_preserves_arguments_working_directory_and_failure
    assert_respond_to VP, :build_run, "build execution must preserve argv and spawn options"
    ENV["VP_BUILD_PRIORITY"] = "normal"
    output = File.join(@dir, "output.json")
    argument = "a b 'quoted' $(must-not-run); &"
    VP.build_run(RbConfig.ruby, "-rjson", "-e",
                 "puts JSON.generate([Dir.pwd, ARGV])", argument, chdir: @dir, out: output)
    assert_equal [File.realpath(@dir), [argument]], JSON.parse(File.read(output))
    error = assert_raises(SystemExit) do
      VP.build_run(RbConfig.ruby, "-e", "exit 17", err: File::NULL)
    end
    assert_equal 17, error.status
  end
end
