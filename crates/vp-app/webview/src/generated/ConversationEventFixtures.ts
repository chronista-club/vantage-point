// 生成物 — 編集しない。SSOT は crates/vantage-point/src/conversation/event.rs、
// 再生成は `cargo test -p vantage-point --test conversation_event_fixtures`。
// Rust が実際に serialize した ConversationEvent（送信形）。`satisfies` で ts-rs 生成型と
// 突き合わせる（tsc --noEmit）= field 名 / kind / 型 / 「TS が Rust より厳しい」向きの必須性を型検査で止める。
// 「TS が緩い」向き（Rust が常に出す field を TS が ? にする）は型では通るので vitest 側で固定する。
import type { EngineConversationEvent } from '../../console'

export const CONVERSATION_EVENT_FIXTURES = {
  codex_message: {
    "kind": "codex_message",
    "item_id": "turn/message",
    "text": "本文",
    "questions": [],
    "append": false
  },
  codex_interactions: {
    "kind": "codex_interactions",
    "requests": [
      {
        "request_id": "codex:fixture:1",
        "kind": "question",
        "title": "質問",
        "details": "",
        "questions": [
          {
            "id": "question-id",
            "header": "対象",
            "question": "どちら？",
            "options": [
              {
                "label": "A",
                "description": "候補"
              }
            ],
            "is_secret": false
          }
        ],
        "blocking": true,
        "can_accept": true
      }
    ]
  },
  codex_interaction_result: {
    "kind": "codex_interaction_result",
    "request_id": "codex:fixture:1",
    "error": null
  },
  codex_config: {
    "kind": "codex_config",
    "config": {
      "models": [],
      "model": null,
      "effort": null,
      "selection": null,
      "error": null
    },
    "request_id": null,
    "error": null
  },
  codex_queue: {
    "kind": "codex_queue",
    "queue": {
      "thread_id": "",
      "turn_id": null,
      "ready": false,
      "items": [],
      "error": null
    },
    "request_id": null,
    "error": null
  },
  codex_history: {
    "kind": "codex_history",
    "thread_id": "codex-thread",
    "events": [
      {
        "kind": "user_message",
        "text": "Console の会話"
      }
    ],
    "user_message_ids": [
      "request-1"
    ],
    "in_flight": false,
    "truncated": true
  },
  session_init_minimal: {
    "kind": "session_init",
    "session_id": "sid-1"
  },
  session_init_full: {
    "kind": "session_init",
    "session_id": "sid-2",
    "model": "claude-fable-5-1",
    "permission_mode": "bypassPermissions",
    "cwd": "/Users/x/repo",
    "tools": [
      "Bash",
      "Read"
    ],
    "mcp_servers": [
      "vantage-point"
    ],
    "slash_commands": [
      "compact"
    ],
    "command_docs": {
      "compact": "会話を圧縮"
    }
  },
  replay_start: {
    "kind": "replay_start"
  },
  replay_end_in_flight: {
    "kind": "replay_end",
    "in_flight": true
  },
  replay_end_idle: {
    "kind": "replay_end",
    "in_flight": false
  },
  user_message: {
    "kind": "user_message",
    "text": "こんにちは"
  },
  message_chunk: {
    "kind": "message_chunk",
    "text": "chunk"
  },
  thought_chunk: {
    "kind": "thought_chunk",
    "text": "thinking…"
  },
  tool_call: {
    "kind": "tool_call",
    "id": "toolu_01",
    "name": "Bash",
    "input": {
      "command": "ls",
      "nested": {
        "a": [
          1,
          2
        ]
      },
      "timeout": 1000
    }
  },
  tool_call_update_ok: {
    "kind": "tool_call_update",
    "tool_use_id": "toolu_01",
    "content": "ok",
    "is_error": false
  },
  tool_call_update_error: {
    "kind": "tool_call_update",
    "tool_use_id": "toolu_02",
    "content": "boom",
    "is_error": true
  },
  subagent_message_text: {
    "kind": "subagent_message",
    "parent_tool_use_id": "toolu_03",
    "role": "text",
    "text": "sub"
  },
  subagent_message_prompt: {
    "kind": "subagent_message",
    "parent_tool_use_id": "toolu_03",
    "role": "prompt",
    "text": "子への指示"
  },
  subagent_message_thinking: {
    "kind": "subagent_message",
    "parent_tool_use_id": "toolu_03",
    "role": "thinking",
    "text": "子の思考"
  },
  plan: {
    "kind": "plan",
    "entries": [
      {
        "content": "調べる",
        "status": "completed"
      },
      {
        "content": "直す",
        "status": "in_progress",
        "active_form": "直している"
      }
    ]
  },
  turn_completed_minimal: {
    "kind": "turn_completed",
    "session_id": "sid-1"
  },
  turn_completed_full: {
    "kind": "turn_completed",
    "session_id": "sid-2",
    "cost_usd": 0.0123,
    "context_tokens": 12345,
    "context_window": 200000
  },
  now_line: {
    "kind": "now_line",
    "text": "panic 箇所を特定中"
  },
  error: {
    "kind": "error",
    "message": "engine error"
  },
  engine_exited: {
    "kind": "engine_exited",
    "message": "exit 0"
  },
  question: {
    "kind": "question",
    "request_id": "req-1",
    "questions": [
      {
        "question": "どちら？",
        "header": "選択",
        "options": [
          {
            "label": "A",
            "description": "説明あり"
          },
          {
            "label": "B",
            "description": ""
          }
        ],
        "multi_select": false
      }
    ]
  },
  permission_request: {
    "kind": "permission_request",
    "request_id": "req-2",
    "tool_name": "Bash",
    "input": {
      "command": "rm -rf /tmp/x"
    }
  },
} satisfies Record<string, EngineConversationEvent>
