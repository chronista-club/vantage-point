# MIDI Mapping 設計

> MIDIコントローラーとVantage Pointアクションのマッピングシステム

## 概要

MIDIコントローラーからの入力をVantage Pointのアクションに
マッピングするシステム。プリセット、設定ファイル、学習モードの
3つの方法でマッピングを定義できる。

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────────┐
│                      The World                               │
│  ┌─────────────────────────────────────────────────────┐    │
│  │                 MIDI Subsystem                       │    │
│  │  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐    │    │
│  │  │    MIDI     │ │   Mapping   │ │   Action    │    │    │
│  │  │   Input     │→│   Engine    │→│  Dispatcher │    │    │
│  │  │  (midir)    │ │             │ │             │    │    │
│  │  └─────────────┘ └─────────────┘ └─────────────┘    │    │
│  │         ↑               ↑               ↓            │    │
│  │         │        ┌──────┴──────┐        │            │    │
│  │  Physical       │             │    View/Park        │    │
│  │  Controller     │  Mappings   │    Operations       │    │
│  │                 │ - Preset    │                      │    │
│  │                 │ - Config    │                      │    │
│  │                 │ - Learned   │                      │    │
│  │                 └─────────────┘                      │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## マッピング方法

### 1. プリセット

デバイス別のデフォルトマッピング:

```rust
struct Preset {
    id: String,
    name: String,
    device_pattern: String,  // デバイス名のパターン
    mappings: Vec<Mapping>,
}
```

**対応デバイス:**
- Akai LPD8
- Akai LPD8 Mk2
- Novation Launchpad
- Arturia MiniLab
- Generic MIDI (汎用)

### 2. 設定ファイル

KDLによるカスタムマッピング定義:

```kdl
// ~/.config/vantage/midi.kdl

midi-config {
    // デバイス指定
    device "LPD8" {
        // パッドマッピング
        pad 1 {
            note 36
            action "workspace_switch" {
                workspace 1
            }
        }
        pad 2 {
            note 37
            action "workspace_switch" {
                workspace 2
            }
        }
        pad 5 {
            note 40
            action "tile_focus" {
                tile 1
            }
        }
        pad 6 {
            note 41
            action "tile_focus" {
                tile 2
            }
        }

        // ノブマッピング
        knob 1 {
            cc 1
            action "panel_resize" {
                panel "left"
                min 100
                max 400
            }
        }
        knob 2 {
            cc 2
            action "panel_resize" {
                panel "right"
                min 100
                max 400
            }
        }

        // モディファイア
        modifier {
            pad 8        // Pad 8を押しながら
            note 43
        }

        // モディファイア付きアクション
        combo "modifier+pad1" {
            action "tile_split" {
                direction "horizontal"
            }
        }
    }
}
```

### 3. 学習モード

GUIでインタラクティブにマッピング:

```
┌─────────────────────────────────────────────────────────┐
│  MIDI Learning Mode                              [Done]  │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  Step 1: Press a pad or turn a knob on your controller  │
│                                                          │
│  Detected: Note 36 (Velocity: 127)                      │
│                                                          │
│  Step 2: Select an action                               │
│                                                          │
│  ┌─────────────────────────────────────────────────┐    │
│  │ ○ Workspace Switch                              │    │
│  │   └─ Workspace: [1 ▼]                           │    │
│  │ ○ Tile Focus                                    │    │
│  │ ○ Tile Split                                    │    │
│  │ ○ Panel Toggle                                  │    │
│  │ ○ Custom Action...                              │    │
│  └─────────────────────────────────────────────────┘    │
│                                                          │
│  [Save Mapping]  [Cancel]                               │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

## データモデル

### Mapping

```rust
struct Mapping {
    id: String,
    source: MidiSource,
    action: Action,
    modifiers: Vec<MidiSource>,
    enabled: bool,
}

enum MidiSource {
    Note { channel: u8, note: u8 },
    CC { channel: u8, controller: u8 },
    PitchBend { channel: u8 },
    Aftertouch { channel: u8 },
}

struct Action {
    action_type: ActionType,
    params: serde_json::Value,
}
```

### ActionType

```rust
enum ActionType {
    // ワークスペース
    WorkspaceSwitch { workspace_id: String },
    WorkspaceNext,
    WorkspacePrev,
    WorkspaceCreate,

    // パネル
    PanelToggle { panel: String },
    PanelResize { panel: String, value: RangeValue },

    // タイル
    TileFocus { tile_index: usize },
    TileSplit { direction: SplitDirection },
    TileClose,
    TileSwap { direction: SwapDirection },
    TileResize { ratio: RangeValue },

    // フローティング
    FloatingToggle { window_id: String },
    FloatingCreate { content_type: String },

    // Paisley Park
    ParkStart { project: String },
    ParkStop { project: String },
    ParkSwitch { project: String },

    // Multiplexer
    MultiplexerDispatch { command: String },
    MultiplexerCancel,

    // カスタム
    Custom { command: String },
    Script { path: String },
}

enum RangeValue {
    Absolute(u8),      // 0-127 → 指定範囲
    Relative(i8),      // 相対値
}
```

## LPD8 プリセット

Akai LPD8のデフォルトマッピング:

### Program 1 (Workspace Mode)

```
┌─────┬─────┬─────┬─────┐
│ WS1 │ WS2 │ WS3 │ WS4 │  Pad 1-4: ワークスペース切替
│ 36  │ 37  │ 38  │ 39  │
├─────┼─────┼─────┼─────┤
│ T1  │ T2  │ T3  │ T4  │  Pad 5-8: タイルフォーカス
│ 40  │ 41  │ 42  │ 43  │
└─────┴─────┴─────┴─────┘

K1 (CC1): Left Panel幅
K2 (CC2): Right Panel幅
K3 (CC3): 分割比率
K4 (CC4): 音量/明るさ
K5 (CC5): 未割り当て
K6 (CC6): 未割り当て
K7 (CC7): 未割り当て
K8 (CC8): 未割り当て
```

### Program 2 (Control Mode)

```
┌─────┬─────┬─────┬─────┐
│Split│Split│Close│Float│  Pad 1-4: タイル操作
│  H  │  V  │     │     │
├─────┼─────┼─────┼─────┤
│ Prj │ Prj │ Prj │ Prj │  Pad 5-8: プロジェクト切替
│  1  │  2  │  3  │  4  │
└─────┴─────┴─────┴─────┘
```

### Program 3 (Multiplexer Mode)

```
┌─────┬─────┬─────┬─────┐
│ Run │Stop │Pause│ All │  Pad 1-4: Multiplexer制御
│     │     │     │     │
├─────┼─────┼─────┼─────┤
│Grp1 │Grp2 │Grp3 │Grp4 │  Pad 5-8: グループ選択
│     │     │     │     │
└─────┴─────┴─────┴─────┘
```

## イベントフロー

### Note On/Off

```
MIDI Controller          The World                    Target
     │                       │                           │
     │ ── Note On ────────→  │                           │
     │    (note=36, vel=127) │                           │
     │                       │ ── lookup mapping ──      │
     │                       │                           │
     │                       │ ── execute action ────→   │
     │                       │    workspace_switch(1)    │
     │                       │                           │
     │                       │ ←── result ──────────     │
     │                       │                           │
     │ ── Note Off ───────→  │                           │
     │    (note=36)          │                           │
     │                       │ (トグル系のみ処理)        │
```

### CC (Continuous Controller)

```
MIDI Controller          The World                    Target
     │                       │                           │
     │ ── CC ─────────────→  │                           │
     │    (cc=1, val=64)     │                           │
     │                       │ ── lookup mapping ──      │
     │                       │                           │
     │                       │ ── map value ────         │
     │                       │    64/127 → 250px         │
     │                       │                           │
     │                       │ ── execute action ────→   │
     │                       │    panel_resize(left,250) │
```

## 設定の優先順位

1. **学習モード** (ユーザーが明示的に設定)
2. **設定ファイル** (~/.config/vantage/midi.kdl)
3. **プリセット** (デバイス検出による自動適用)

同一ソースに複数のマッピングがある場合、上位が優先。

## MCP Tools

```json
{
    "name": "midi_list_devices",
    "description": "接続中のMIDIデバイス一覧"
}
```

```json
{
    "name": "midi_set_mapping",
    "description": "MIDIマッピングを設定",
    "inputSchema": {
        "properties": {
            "source": {
                "type": "object",
                "properties": {
                    "type": { "enum": ["note", "cc"] },
                    "channel": { "type": "number" },
                    "number": { "type": "number" }
                }
            },
            "action": { "type": "string" },
            "params": { "type": "object" }
        }
    }
}
```

```json
{
    "name": "midi_learn",
    "description": "学習モードを開始",
    "inputSchema": {
        "properties": {
            "action": { "type": "string" },
            "params": { "type": "object" }
        }
    }
}
```

## 状態永続化

マッピング設定はVantage DBに保存:

```sql
-- SurrealDB Schema
DEFINE TABLE midi_mappings SCHEMAFULL;
DEFINE FIELD id ON midi_mappings TYPE string;
DEFINE FIELD device_name ON midi_mappings TYPE string;
DEFINE FIELD source ON midi_mappings TYPE object;
DEFINE FIELD action ON midi_mappings TYPE object;
DEFINE FIELD modifiers ON midi_mappings TYPE array;
DEFINE FIELD priority ON midi_mappings TYPE int;
DEFINE FIELD created_at ON midi_mappings TYPE datetime;
```

## 関連ドキュメント

- [spec/03-lpd8-integration.md](../spec/03-lpd8-integration.md)
- [design/11-view-system.md](./11-view-system.md)
