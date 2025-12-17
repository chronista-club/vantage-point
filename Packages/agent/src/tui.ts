/**
 * Vantage Agent - TypeScript TUI CLI
 *
 * Claude Agent SDK + MCP を使った完全なTypeScript版TUI
 * Claude Code同様のファイルアクセスが可能
 */

import { query, type Options } from "@anthropic-ai/claude-code";
import chalk from "chalk";
import * as readline from "readline";
import * as path from "path";
import * as fs from "fs";

// Configuration
interface AgentConfig {
  cwd: string;
  claudeMdPath?: string;
  mcpServers: Options["mcpServers"];
}

// Load CLAUDE.md if exists
function loadClaudeMd(projectDir: string): string | undefined {
  const claudeMdPath = path.join(projectDir, "CLAUDE.md");
  if (fs.existsSync(claudeMdPath)) {
    return fs.readFileSync(claudeMdPath, "utf-8");
  }
  return undefined;
}

// Create agent configuration
function createConfig(projectDir: string): AgentConfig {
  const claudeMd = loadClaudeMd(projectDir);

  return {
    cwd: projectDir,
    claudeMdPath: claudeMd ? path.join(projectDir, "CLAUDE.md") : undefined,
    mcpServers: {
      "creo-memories": {
        command: "bun",
        args: ["run", "/Users/makoto/repos/creo-memories/packages/mcp-server/src/index.ts"],
        env: {
          CREO_API_URL: process.env.CREO_API_URL || "http://localhost:3000",
        },
      },
    },
  };
}

// Chat function
async function chat(
  userMessage: string,
  config: AgentConfig,
  systemPrompt?: string
): Promise<void> {
  console.log(chalk.dim(`\n${chalk.green("You:")} ${userMessage}\n`));

  try {
    let fullResponse = "";

    // Build query options
    const queryOptions: Options = {
      cwd: config.cwd,
      mcpServers: config.mcpServers,
      maxTurns: 10,
      // Enable all tools like Claude Code
      allowedTools: [
        "Read",
        "Write",
        "Edit",
        "Bash",
        "Glob",
        "Grep",
        "LS",
        "mcp__creo-memories__*",
      ],
    };

    // Add system prompt if CLAUDE.md exists
    if (systemPrompt) {
      queryOptions.appendSystemPrompt = systemPrompt;
    }

    for await (const message of query({
      prompt: userMessage,
      options: queryOptions,
    })) {
      // Handle different message types based on SDK
      if (message.type === "assistant") {
        // Assistant message contains text and tool_use blocks
        if (message.message?.content) {
          for (const block of message.message.content) {
            if (block.type === "text") {
              process.stdout.write(chalk.white(block.text));
              fullResponse += block.text;
            } else if (block.type === "tool_use") {
              // Tool use is in assistant message
              console.log(chalk.magenta(`\n🔧 ${block.name} を実行中...`));
            }
          }
        }
      }

      // System init message shows available tools
      if (message.type === "system" && message.subtype === "init") {
        console.log(chalk.dim(`Model: ${message.model}`));
        console.log(chalk.dim(`Tools: ${message.tools?.length || 0}`));
        if (message.mcp_servers?.length) {
          console.log(chalk.dim(`MCP: ${message.mcp_servers.map(s => s.name).join(", ")}`));
        }
      }

      // Final result
      if (message.type === "result") {
        if (message.subtype === "success" && message.result) {
          if (message.result !== fullResponse) {
            console.log(chalk.white(`\n${message.result}`));
          }
        } else if (message.subtype === "error_during_execution") {
          console.log(chalk.red(`\nError during execution`));
        }
      }
    }
  } catch (error) {
    console.error(chalk.red("\nError:"), error);
  }

  console.log();
}

// Main
async function main(): Promise<void> {
  // Get project directory from args or current directory
  const projectDir = process.argv[2]
    ? path.resolve(process.argv[2])
    : process.cwd();

  console.log(chalk.bold.blue("\n🎯 Vantage Agent TUI\n"));
  console.log(chalk.gray(`Project: ${projectDir}`));
  console.log(chalk.gray("Claude Agent SDK + MCP"));

  // Load config
  const config = createConfig(projectDir);

  // Load CLAUDE.md as system prompt
  let systemPrompt: string | undefined;
  if (config.claudeMdPath) {
    systemPrompt = loadClaudeMd(projectDir);
    console.log(chalk.green(`✓ CLAUDE.md loaded`));
  } else {
    console.log(chalk.yellow("⚠ No CLAUDE.md found"));
  }

  console.log(chalk.gray("Type 'quit' to exit, 'clear' to clear screen\n"));

  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
  });

  const prompt = (): void => {
    rl.question(chalk.cyan("You: "), async (input) => {
      const trimmed = input?.trim();

      if (!trimmed) {
        prompt();
        return;
      }

      if (trimmed.toLowerCase() === "quit" || trimmed.toLowerCase() === "exit") {
        console.log(chalk.blue("\nGoodbye!\n"));
        rl.close();
        return;
      }

      if (trimmed.toLowerCase() === "clear") {
        console.clear();
        console.log(chalk.bold.blue("🎯 Vantage Agent TUI\n"));
        prompt();
        return;
      }

      await chat(trimmed, config, systemPrompt);
      prompt();
    });
  };

  prompt();
}

main().catch(console.error);
