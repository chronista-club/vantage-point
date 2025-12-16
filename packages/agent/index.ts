/**
 * Vantage Point Agent CLI
 *
 * AI主導の選択肢UIで対話を進める
 */

import Anthropic from "@anthropic-ai/sdk";
import prompts from "prompts";
import chalk from "chalk";

// Types
interface Choice {
  id: string;
  label: string;
  description?: string;
}

interface ChoicePrompt {
  message: string;
  choices: Choice[];
}

type CooperationMode = "cooperative" | "delegated" | "autonomous";

// State
let currentMode: CooperationMode = "cooperative";
const conversationHistory: { role: "user" | "assistant"; content: string }[] = [];

// Claude SDK client - ANTHROPIC_API_KEY 環境変数から自動読み込み
const client = new Anthropic();

// System prompt that instructs Claude to provide choices
const SYSTEM_PROMPT = `あなたはVantage Pointの開発アシスタントです。

## 対話スタイル
ユーザーとの対話では、必ず**選択肢形式**で次のアクションを提示してください。

## 出力フォーマット
回答の最後に、必ず以下のJSON形式で選択肢を出力してください：

\`\`\`choices
{
  "message": "次のステップはどうしますか？",
  "choices": [
    {"id": "A", "label": "選択肢A", "description": "説明"},
    {"id": "B", "label": "選択肢B", "description": "説明"},
    {"id": "C", "label": "選択肢C", "description": "説明"}
  ]
}
\`\`\`

## ルール
- 選択肢は2〜4個程度
- 各選択肢には簡潔なラベルと説明を含める
- ユーザーが選択肢以外の自由入力をした場合も対応する
- 協調モードに応じて対話スタイルを調整する

## 現在の協調モード
`;

// Parse choices from Claude's response
function parseChoices(response: string): ChoicePrompt | null {
  const match = response.match(/```choices\n([\s\S]*?)\n```/);
  if (!match) return null;

  try {
    return JSON.parse(match[1]) as ChoicePrompt;
  } catch {
    return null;
  }
}

// Remove choices block from response for display
function cleanResponse(response: string): string {
  return response.replace(/```choices\n[\s\S]*?\n```/g, "").trim();
}

// Get mode description
function getModeDescription(mode: CooperationMode): string {
  switch (mode) {
    case "cooperative": return "協調 - ユーザーと一緒に進める";
    case "delegated": return "委任 - 任せて、途中経過・結果を確認";
    case "autonomous": return "自律 - 完全に任せる";
  }
}

// Main chat function using Anthropic SDK
async function chat(userMessage: string): Promise<{ text: string; choices: ChoicePrompt | null }> {
  conversationHistory.push({ role: "user", content: userMessage });

  const response = await client.messages.create({
    model: "claude-3-haiku-20240307",  // 低コストモデル
    max_tokens: 1024,
    system: SYSTEM_PROMPT + getModeDescription(currentMode),
    messages: conversationHistory,
  });

  const assistantMessage = response.content[0].type === "text"
    ? response.content[0].text
    : "";

  conversationHistory.push({ role: "assistant", content: assistantMessage });

  const choices = parseChoices(assistantMessage);
  const cleanText = cleanResponse(assistantMessage);

  return { text: cleanText, choices };
}

// Display choices and get user selection
async function promptChoices(prompt: ChoicePrompt): Promise<string> {
  console.log(chalk.cyan(`\n${prompt.message}`));

  const { selection } = await prompts({
    type: "select",
    name: "selection",
    message: "選択してください",
    choices: [
      ...prompt.choices.map(c => ({
        title: `${chalk.bold(c.id)} ${c.label}`,
        description: c.description,
        value: c.id,
      })),
      { title: chalk.gray("テキスト入力"), value: "__text__" },
    ],
  });

  if (selection === "__text__") {
    const { text } = await prompts({
      type: "text",
      name: "text",
      message: "入力してください",
    });
    return text || "";
  }

  return `${selection}を選択`;
}

// Mode switcher
async function switchMode(): Promise<void> {
  const { mode } = await prompts({
    type: "select",
    name: "mode",
    message: "協調モードを選択",
    choices: [
      { title: "協調 - ユーザーと一緒に進める", value: "cooperative" },
      { title: "委任 - 任せて、途中経過・結果を確認", value: "delegated" },
      { title: "自律 - 完全に任せる", value: "autonomous" },
    ],
  });
  currentMode = mode;
  console.log(chalk.green(`モードを「${getModeDescription(currentMode)}」に変更しました`));
}

// Main loop
async function main() {
  console.log(chalk.bold.blue("\n🎯 Vantage Point Agent\n"));
  console.log(chalk.gray("開発行為を拡張する - AI主導の選択肢UI"));
  console.log(chalk.gray("(Anthropic SDK + API Key)"));
  console.log(chalk.gray(`現在のモード: ${getModeDescription(currentMode)}`));
  console.log(chalk.gray("コマンド: /mode (モード変更), /quit (終了)\n"));

  // Initial prompt
  let userInput = "こんにちは。開発を始めましょう。";

  while (true) {
    try {
      console.log(chalk.dim(`\nユーザー: ${userInput}\n`));

      const { text, choices } = await chat(userInput);

      console.log(chalk.white(text));

      if (choices) {
        userInput = await promptChoices(choices);
      } else {
        // No choices, ask for free input
        const { input } = await prompts({
          type: "text",
          name: "input",
          message: "あなた",
        });

        if (!input) break;

        if (input === "/quit") break;
        if (input === "/mode") {
          await switchMode();
          continue;
        }

        userInput = input;
      }

      if (!userInput) break;

    } catch (error) {
      if (error instanceof Error && error.message.includes("canceled")) {
        break;
      }
      console.error(chalk.red("エラー:"), error);
      break;
    }
  }

  console.log(chalk.blue("\nさようなら！\n"));
}

main().catch(console.error);
