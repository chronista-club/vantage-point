# CLAUDE.md

## Project Overview

Vantage Point for macOS - Native menu bar application for managing Vantage Point Processes via TheWorld API.

## Tech Stack

- **Language**: Swift 5.9+
- **UI**: SwiftUI (NSPopover + NSStatusItem)
- **Minimum OS**: macOS 13.0 (Ventura)

## Project Structure

```
vantage-point-mac/
├── VantagePoint/
│   ├── Package.swift              # Swift PM manifest
│   └── Sources/
│       ├── VantagePointApp.swift   # App entry point
│       ├── AppDelegate.swift       # Menu bar management
│       ├── TheWorldClient.swift    # TheWorld API client (port 32000)
│       ├── TheWorldTypes.swift     # API response types
│       ├── PopoverViewModel.swift  # Popover state management
│       ├── PopoverView.swift       # Main popover UI
│       ├── ProjectRowView.swift    # Project row component
│       ├── ProjectSettingsView.swift # Project settings UI
│       ├── ConfigManager.swift     # config.toml management
│       ├── ProcessManager.swift    # Port-scan fallback (legacy)
│       ├── BonjourBrowser.swift    # Bonjour discovery
│       ├── UpdateService.swift     # Auto-update service
│       ├── UpdateAlertView.swift   # Update alert UI
│       └── UserPromptService.swift # User prompt handling
├── docs/
│   └── spec/                       # Spec documents
└── README.md
```

## Key Components

### AppDelegate
- NSStatusItem setup
- NSPopover management
- DistributedNotification listener (`club.chronista.vp.process.changed`)

### TheWorldClient
- Communicates with TheWorld daemon (port 32000)
- Process list, start, stop, open operations
- Rust 側の ProcessManagerCapability と対応

### PopoverViewModel
- TheWorld API ベースのプロジェクト状態管理
- Config + running process のマージ表示

### ProcessManager (Actor, legacy fallback)
- Port scanning (33000-33010)
- Health check via /api/health
- TheWorld 不在時のフォールバック

## Build Commands

```bash
cd VantagePoint
swift build           # Debug build
swift build -c release  # Release build
swift run             # Build and run
```

## Port Configuration

- TheWorld: 32000 (HTTP)
- Project Processes: 33000-33010 (HTTP + WS)

## Related Issues

- #1: メニューバー基盤 (this implementation)
- #2: Bonjour発見
- #3: 通知・Shortcuts
