// vp_bridge.h — VP Bridge FFI ヘッダー
// ratatui NativeBackend の C ABI インターフェース
//
// Swift 側: module.modulemap 経由で import VPBridge
// 生成元: crates/vp-bridge/src/ffi.rs

#ifndef VP_BRIDGE_H
#define VP_BRIDGE_H

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

// =============================================================================
// 構造体
// =============================================================================

/// Cell データ（1セルの描画情報）
typedef struct {
    /// UTF-8 文字列（最大 4 バイト + null 終端）
    uint8_t ch[5];
    /// 前景色 (RGBA: 0xRRGGBBAA)
    uint32_t fg;
    /// 背景色 (RGBA: 0xRRGGBBAA)
    uint32_t bg;
    /// スタイルフラグ
    ///   bit 0: bold
    ///   bit 1: italic
    ///   bit 2: underline
    ///   bit 3: inverse
    ///   bit 4: strikethrough
    ///   bit 5: dim
    ///   bit 6: wide char (VT parser WIDE_CHAR flag)
    uint8_t flags;
} VPCellData;

/// カーソル情報
typedef struct {
    uint16_t x;
    uint16_t y;
    bool visible;
} VPCursorInfo;

/// フレーム更新コールバック型
typedef void (*VPFrameReadyCallback)(void);

// =============================================================================
// セッション管理（マルチウィンドウ対応）
// =============================================================================

/// セッションを作成して ID を返す
/// @param width  グリッド幅（列数）
/// @param height グリッド高さ（行数）
/// @param frame_callback フレーム更新時に呼ばれるコールバック（nullable）
/// @return セッション ID (0 = 失敗)
uint32_t vp_bridge_create(uint16_t width, uint16_t height, VPFrameReadyCallback frame_callback);

/// セッションを破棄
/// @param session_id 対象セッション ID
void vp_bridge_destroy(uint32_t session_id);

// =============================================================================
// ライフサイクル（後方互換 — セッション ID 1 を暗黙使用）
// =============================================================================

/// Backend を初期化（後方互換: セッション 1 を使用）
void vp_bridge_init(uint16_t width, uint16_t height, VPFrameReadyCallback frame_callback);

/// Backend を破棄（後方互換: セッション 1 を使用）
void vp_bridge_deinit(void);

// =============================================================================
// 状態操作（セッション指定）
// =============================================================================

/// グリッドサイズを変更（セッション指定）
void vp_bridge_resize_session(uint32_t session_id, uint16_t width, uint16_t height);

/// 指定座標の CellData を取得（セッション指定）
VPCellData vp_bridge_get_cell_session(uint32_t session_id, uint16_t x, uint16_t y);

/// 現在のグリッドサイズを取得（セッション指定）
void vp_bridge_get_size_session(uint32_t session_id, uint16_t *out_width, uint16_t *out_height);

/// カーソル情報を取得（セッション指定）
VPCursorInfo vp_bridge_get_cursor_session(uint32_t session_id);

/// バッファ全体を一括取得（セッション指定）
uint32_t vp_bridge_get_buffer_session(uint32_t session_id, VPCellData *dst, uint32_t max_cells);

// =============================================================================
// 状態操作（後方互換 — セッション ID 1 を暗黙使用）
// =============================================================================

void vp_bridge_resize(uint16_t width, uint16_t height);
VPCellData vp_bridge_get_cell(uint16_t x, uint16_t y);
void vp_bridge_get_size(uint16_t *out_width, uint16_t *out_height);
VPCursorInfo vp_bridge_get_cursor(void);
uint32_t vp_bridge_get_buffer(VPCellData *dst, uint32_t max_cells);

// =============================================================================
// PTY 操作（セッション指定）
// =============================================================================

/// PTY を起動（セッション指定）
int32_t vp_bridge_pty_start_session(uint32_t session_id, const char *cwd, uint16_t cols, uint16_t rows);

/// コマンド指定で PTY を起動（セッション指定）
/// @param command 実行コマンド文字列（NULL ならデフォルトシェル）
int32_t vp_bridge_pty_start_command_session(uint32_t session_id, const char *cwd, const char *command, uint16_t cols, uint16_t rows);

/// PTY にバイト列を送信（セッション指定）
int32_t vp_bridge_pty_write_session(uint32_t session_id, const uint8_t *data, uint32_t len);

/// PTY が稼働中か（セッション指定）
bool vp_bridge_pty_is_running_session(uint32_t session_id);

/// PTY を停止（セッション指定）
void vp_bridge_pty_stop_session(uint32_t session_id);

/// スクロールバック表示位置を変更（セッション指定）
/// @param session_id 対象セッション ID
/// @param delta > 0: 上（過去）, < 0: 下（現在）, INT32_MAX: ページアップ, INT32_MIN: ページダウン
void vp_bridge_scroll_session(uint32_t session_id, int32_t delta);

// =============================================================================
// PTY 操作（後方互換 — セッション ID 1 を暗黙使用）
// =============================================================================

int32_t vp_bridge_pty_start(const char *cwd, uint16_t cols, uint16_t rows);
int32_t vp_bridge_pty_write(const uint8_t *data, uint32_t len);
bool vp_bridge_pty_is_running(void);
void vp_bridge_pty_stop(void);

// =============================================================================
// クローム（ヘッダー/フッター）
// =============================================================================

/// クローム領域を設定（ヘッダー/フッターの行数）
/// PTY には height - header - footer 行が通知される
void vp_bridge_set_chrome(uint32_t session_id, uint16_t header_rows, uint16_t footer_rows);

/// クローム行にテキストを書き込む
/// @param y グリッド上の絶対行番号（0 = 最上行）
/// @param text UTF-8 文字列
/// @param fg 前景色 RGBA (0 = デフォルト)
/// @param bg 背景色 RGBA (0 = デフォルト)
void vp_bridge_write_chrome_line(uint32_t session_id, uint16_t y, const char *text, uint32_t fg, uint32_t bg);

// =============================================================================
// テスト・ユーティリティ
// =============================================================================

void vp_bridge_draw_test_pattern(void);
const char* vp_bridge_version(void);

#ifdef __cplusplus
}
#endif

#endif // VP_BRIDGE_H
