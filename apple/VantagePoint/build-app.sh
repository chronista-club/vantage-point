#!/bin/bash
# VantagePoint.app ビルドスクリプト
#
# 使い方:
#   ./build-app.sh          # Debug ビルド
#   ./build-app.sh release  # Release ビルド
#
# 出力: .build/VantagePoint.app

set -euo pipefail
cd "$(dirname "$0")"

MODE="${1:-debug}"
XCODE_CONFIG="Debug"
CARGO_PROFILE=""

if [ "$MODE" = "release" ]; then
    XCODE_CONFIG="Release"
    CARGO_PROFILE="--release"
fi

# 1. Rust staticlib ビルド
echo "🔨 Building vp-bridge ($MODE)..."
(cd ../.. && cargo build -p vp-bridge $CARGO_PROFILE)

# 2. xcodegen でプロジェクト生成
echo "⚙️  Generating Xcode project..."
xcodegen generate --quiet

# 3. xcodebuild
echo "🔨 Building VantagePoint ($MODE)..."
xcodebuild -project VantagePoint.xcodeproj \
    -scheme VantagePoint \
    -configuration "$XCODE_CONFIG" \
    build \
    -quiet

# 4. ビルド成果物をコピー
DERIVED_DATA="$HOME/Library/Developer/Xcode/DerivedData"
APP_SRC=$(find "$DERIVED_DATA" -path "*/VantagePoint-*/Build/Products/$XCODE_CONFIG/VantagePoint.app" -maxdepth 8 2>/dev/null | grep -v Index.noindex | head -1)

if [ -z "$APP_SRC" ]; then
    echo "❌ VantagePoint.app not found in DerivedData"
    exit 1
fi

APP_DIR=".build/VantagePoint.app"
rm -rf "$APP_DIR"
mkdir -p .build
cp -R "$APP_SRC" "$APP_DIR"

echo "✅ VantagePoint.app built: $APP_DIR"
echo ""
echo "起動: open $APP_DIR"
echo "インストール: cp -R $APP_DIR ~/Applications/"
