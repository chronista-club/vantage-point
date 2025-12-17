/**
 * Vantage Agent - CLI (for testing)
 */

import { query } from "@anthropic-ai/claude-code";
import chalk from "chalk";
import * as readline from "readline";

const rl = readline.createInterface({
  input: process.stdin,
  output: process.stdout,
});

// MCP Servers configuration
const mcpServers = {
  "creo-memories": {
    command: "bun",
    args: ["run", "/Users/makoto/repos/creo-memories/packages/mcp-server/src/index.ts"],
    env: {
      CREO_API_URL: process.env.CREO_API_URL || "http://localhost:3000",
    },
  },
};

async function chat(userMessage: string): Promise<void> {
  console.log(chalk.dim(`\nUser: ${userMessage}\n`));

  try {
    for await (const message of query({
      prompt: userMessage,
      options: {
        mcpServers,
        maxTurns: 10,
      },
    })) {
      if (message.type === "assistant") {
        if (message.message?.content) {
          for (const block of message.message.content) {
            if (block.type === "text") {
              process.stdout.write(chalk.white(block.text));
            }
          }
        }
      }

      if (message.type === "tool_use") {
        console.log(chalk.magenta(`\n🔧 ${message.name || "tool"} を実行中...`));
      }

      if (message.type === "tool_result") {
        const preview = typeof message.content === "string"
          ? message.content.slice(0, 100)
          : JSON.stringify(message.content).slice(0, 100);
        console.log(chalk.green(`✓ ${preview}`));
      }

      if (message.type === "result" && message.subtype === "success") {
        console.log(chalk.white(`\n${message.result}`));
      }
    }
  } catch (error) {
    console.error(chalk.red("Error:"), error);
  }

  console.log();
}

async function main(): Promise<void> {
  console.log(chalk.bold.blue("\n🎯 Vantage Agent CLI\n"));
  console.log(chalk.gray("Claude Agent SDK + MCP"));
  console.log(chalk.gray("Type 'quit' to exit\n"));

  const prompt = (): void => {
    rl.question(chalk.cyan("You: "), async (input) => {
      if (!input || input.toLowerCase() === "quit") {
        console.log(chalk.blue("\nGoodbye!\n"));
        rl.close();
        return;
      }

      await chat(input);
      prompt();
    });
  };

  prompt();
}

main().catch(console.error);
