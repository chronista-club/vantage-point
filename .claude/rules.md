# 三言語対応ルール / Trilingual Documentation Rules / 삼국어 대응 규칙

このファイルは、本プロジェクトのドキュメント作成における三言語対応ルールを定義します。

## 対象言語 / Target Languages / 대상 언어

1. 日本語 (Japanese)
2. 英語 (English)
3. 韓国語 (Korean)

## 適用範囲 / Scope / 적용 범위

### 三言語対応が必要なドキュメント / Documents Requiring Trilingual Support / 삼국어 대응이 필요한 문서

- README.md
- CONTRIBUTING.md
- コードコメント（重要な関数・クラスの説明）/ Code comments (important functions/classes) / 코드 주석 (중요한 함수/클래스 설명)
- エラーメッセージ / Error messages / 에러 메시지
- ユーザー向けドキュメント / User-facing documentation / 사용자 대상 문서

### 例外 / Exceptions / 예외

- **CLAUDE.md** - 日本語のみ / Japanese only / 일본어만
- 内部技術文書 / Internal technical documents / 내부 기술 문서
- Git コミットメッセージ / Git commit messages / Git 커밋 메시지

## 形式 / Format / 형식

### ドキュメントファイル / Documentation Files / 문서 파일

```markdown
# タイトル / Title / 제목

## セクション / Section / 섹션

内容（日本語）

Content (English)

내용 (한국어)
```

### コードコメント / Code Comments / 코드 주석

```swift
/// 関数の説明（日本語）
/// Function description (English)
/// 함수 설명 (한국어)
func exampleFunction() {
    // 実装
}
```

### エラーメッセージ / Error Messages / 에러 메시지

```swift
enum LocalizedError {
    static let networkError = NSLocalizedString(
        "ネットワークエラーが発生しました / Network error occurred / 네트워크 오류가 발생했습니다",
        comment: "Network error message"
    )
}
```

## 記述順序 / Order of Languages / 기술 순서

1. 日本語 / Japanese / 일본어
2. 英語 / English / 영어
3. 韓国語 / Korean / 한국어

## 翻訳ガイドライン / Translation Guidelines / 번역 가이드라인

### 技術用語 / Technical Terms / 기술 용어

- 一般的な技術用語は各言語の標準的な表現を使用 / Use standard expressions for common technical terms / 일반적인 기술 용어는 각 언어의 표준 표현 사용
- Apple固有の用語は公式ドキュメントに準拠 / Follow official documentation for Apple-specific terms / Apple 고유 용어는 공식 문서 준수

### トーン / Tone / 톤

- 日本語: 丁寧語を使用 / Japanese: Use polite form / 일본어: 정중어 사용
- 英語: 明確で簡潔な表現 / English: Clear and concise / 영어: 명확하고 간결한 표현
- 韓国語: 敬語を適切に使用 / Korean: Use appropriate honorifics / 한국어: 경어 적절히 사용

## 実装例 / Implementation Example / 구현 예

```markdown
# Vantage

## 概要 / Overview / 개요

VantageはApple Vision Pro向けの没入型アプリケーションです。

Vantage is an immersive application for Apple Vision Pro.

Vantage는 Apple Vision Pro용 몰입형 애플리케이션입니다.

## 機能 / Features / 기능

- カスタムMetalレンダリング / Custom Metal rendering / 커스텀 Metal 렌더링
- ARKit統合 / ARKit integration / ARKit 통합
- RealityKit対応 / RealityKit support / RealityKit 지원
```

## メンテナンス / Maintenance / 유지보수

- 新機能追加時は必ず三言語でドキュメントを更新 / Always update documentation in all three languages when adding new features / 새 기능 추가 시 반드시 삼국어로 문서 업데이트
- 翻訳の一貫性を保つため、用語集を維持 / Maintain a glossary to ensure translation consistency / 번역 일관성을 위해 용어집 유지