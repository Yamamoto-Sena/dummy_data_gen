# コーディング基準書

| バージョン | 作成日 | 備考 |
|-----------|--------|------|
| 1.0 | 2026-03-01 | 初版 |
| 1.1 | 2026-03-28 | P02再定義、P04/P06緩和、P20明確化、C1/C2分離、例外一覧追加 |
| 1.2 | 2026-06-12 | 補完ツール（Modern Web Guidance）セクション追加、Lintヒント基盤（`lint/`）への参照追加 |

---

## 目次

- [概要](#概要)
- [カテゴリ A: 型安全性（TypeScript）](#カテゴリ-a-型安全性typescript)
  - [P01: `any` の使用禁止](#p01-any-の使用禁止)
  - [P02: 型アサーション（`as`）の制限](#p02-型アサーションas-の制限)
  - [P03: 非 null アサーション（`!`）の制限](#p03-非-null-アサーション-の制限)
- [カテゴリ B: コンポーネント設計（React）](#カテゴリ-b-コンポーネント設計react)
  - [P04: プレゼンテーション / ロジック分離](#p04-プレゼンテーション--ロジック分離)
  - [P05: props の型定義](#p05-props-の型定義)
  - [P06: コンポーネントの単一責任](#p06-コンポーネントの単一責任)
- [カテゴリ C: 非同期・副作用（React）](#カテゴリ-c-非同期副作用react)
  - [P07: `useEffect` 内の非同期処理](#p07-useeffect-内の非同期処理)
  - [P08: エラーハンドリング（Promise rejection の伝播）](#p08-エラーハンドリングpromise-rejection-の伝播)
  - [P09: クリーンアップ関数](#p09-クリーンアップ関数)
- [カテゴリ D: API 通信（FE/BE 共通）](#カテゴリ-d-api-通信febe-共通)
  - [P10: エラーレスポンスの形式統一](#p10-エラーレスポンスの形式統一)
  - [P11: HTTP ステータスコードの使い方](#p11-http-ステータスコードの使い方)
- [カテゴリ E: レイヤー分離（FastAPI）](#カテゴリ-e-レイヤー分離fastapi)
  - [P12: routers / services / repositories の責務分離](#p12-routers--services--repositories-の責務分離)
  - [P13: 依存性注入（`Depends`）の使い方](#p13-依存性注入depends-の使い方)
- [カテゴリ F: バリデーション（Pydantic / FastAPI）](#カテゴリ-f-バリデーションpydantic--fastapi)
  - [P14: バリデーションはシステム境界（routers）で行う](#p14-バリデーションはシステム境界routers-で行う)
  - [P15: Request / Response Pydantic モデルの分離](#p15-request--response-pydantic-モデルの分離)
- [カテゴリ G: DB・トランザクション（SQLAlchemy）](#カテゴリ-g-dbトランザクションsqlalchemy)
  - [P16: セッションの Unit of Work パターン](#p16-セッションの-unit-of-work-パターン)
  - [P17: N+1 クエリの回避](#p17-n1-クエリの回避)
  - [P18: Alembic マイグレーションの運用ルール](#p18-alembic-マイグレーションの運用ルール)
- [カテゴリ H: LLM 固有のアンチパターン](#カテゴリ-h-llm-固有のアンチパターン)
  - [P19: 型システム迂回パターン](#p19-型システム迂回パターン)
  - [P20: スタブ・未完成実装](#p20-スタブ未完成実装)
  - [P21: デバッグコード・一時コードの残置](#p21-デバッグコード一時コードの残置)
  - [P22: SQLインジェクション防止](#p22-sqlインジェクション防止)
- [採点基準](#採点基準)
- [例外一覧](#例外一覧)

---

## 概要

<!-- プロジェクトに合わせて技術スタックを記載 -->
本書はフロントエンド（React 18 + TypeScript 5 + Tailwind CSS + shadcn/ui）およびバックエンド（FastAPI + Pydantic v2 + SQLAlchemy 2.x + PostgreSQL）のコーディング基準を定める。

各原則は YES/NO の二値で判定できる形式で記述されており、コードレビューおよびセルフチェックに使用する。

### 機械的強制の対応状況

<!-- プロジェクトの Lint 設定に合わせて更新 -->

| 原則 | Lint ルール | 強制レベル | 備考 |
|------|-----------|-----------|------|
| P01 `any` | `@typescript-eslint/no-explicit-any` | **error** | 既存（recommended） |
| P02 `as` | `@typescript-eslint/consistent-type-assertions` | **warn** | 段階修正後 error 昇格 |
| P03 `!` | `@typescript-eslint/no-non-null-assertion` | **warn** | 段階修正後 error 昇格 |
| P19 `# type: ignore` | ruff `PGH003` | **error** | |
| P19 `Any` 引数 | ruff `ANN401` | **error** | |
| P20 TODO/FIXME | eslint `no-warning-comments` / ruff `FIX` | **error** | |
| P21 `console.log` | eslint `no-console` | **error** | `warn`, `error` は許可 |
| P21 `print()` | ruff `T201` | **error** | scripts/tests は除外 |
| P22 SQL injection | ruff `S608` | **error** | |
| P04-P18 他 | — | レビュー依存 | 意味的判断が必要なため Lint 化困難 |

> 上記の設定ファイル実体（ESLint/ruff スニペット・修正ヒント辞書・編集直後フィードバック用 hook）は、同じテンプレートフォルダ内の `lint/` に同梱している。導入手順は `lint/README.md` を参照。Lint エラーには P規約ID と修正方針が付くため、LLM は Lint 出力だけで自己修正できる。

---

## カテゴリ A: 型安全性（TypeScript）

### P01: `any` の使用禁止

**説明**: `any` 型をコード内で使用してはならない。型が不確定な値には `unknown` を使用し、型ガードで絞り込む。

**このスタックで特に重要な理由**:
TypeScript の `strict: true` オプションは `noImplicitAny` を含み、暗黙的な `any` をコンパイルエラーとする。ただし明示的な `any` は `strict: true` であっても許容されるため、人的ルールで補う必要がある。TypeScript 公式ドキュメントは `unknown` について「`unknown` is the type-safe counterpart of `any`」と定義し、`unknown` の値は型ガードで絞り込むまで操作できない設計になっている（参照: [TypeScript Handbook — Everyday Types](https://www.typescriptlang.org/docs/handbook/2/everyday-types.html)）。API レスポンスの型変換や外部ライブラリとの結合点で型不明データが生じやすく、誤った `any` 使用はそのような境界での型安全性を完全に破壊する。

**チェックルール**:
- YES: `any` がコード中に存在しない。型不確定な場合は `unknown` を用い、`typeof` / `instanceof` / ユーザー定義型ガード（`is` 述語）で絞り込んでいる。
- NO（違反）: `someValue as any`、`: any`、`(obj: any)` のように `any` が明示的に記述されている。
- 例外: サードパーティライブラリの型定義が存在しない場合に限り使用を許可する。その際はインラインコメント `// eslint-disable-next-line @typescript-eslint/no-explicit-any — <理由>` を付与し、理由が明示されていること。

---

### P02: 安全でない型キャスト（unsafe narrowing）の禁止

**説明**: 実行時に型が保証されない `as SomeType` によるダウンキャスト（unsafe narrowing）を禁止する。型ガードまたは型推論で型を確定させること。ただし、安全なアサーション（widening、DOM 境界、リテラル型変換）は許可する。

**このスタックで特に重要な理由**:
TypeScript 公式ハンドブックは型アサーションについて「Sometimes this rule can be too conservative and will disallow more complex coercions that might be valid」と述べ、アサーションはコンパイラを「黙らせる」手段に過ぎないことを明示している（参照: [TypeScript Handbook — Everyday Types](https://www.typescriptlang.org/docs/handbook/2/everyday-types.html)）。型アサーションはランタイムに何ら変換を行わないため、実際の値が期待する型でない場合、実行時エラーが発生するまでバグが潜伏する。API レスポンスをフロントエンドで受け取る箇所が多く、ここでの誤ったアサーションはデータの整合性崩壊に直結する。

**チェックルール**:
- YES: 型を確定するために `typeof` / `instanceof` / ユーザー定義型ガード（`function isX(v: unknown): v is X`）を使用している。
- NO（違反）: `const user = response as User` のように、API レスポンスや外部データに対して型ガードを経由せず `as` で型をキャストしている。
- 許可されるケース（違反としない）:
  - `as const`（リテラル型変換）
  - DOM API 操作（例: `e.target as HTMLInputElement`）— TypeScript がより一般的な型を返す標準 API
  - widening アサーション（例: `value as string | number` — 型を広げる方向のキャスト）

> **P19 との関係**: `as unknown as T`（ダブルキャスト）は P02 ではなく P19（型システム迂回パターン）で禁止している。

---

### P03: 非 null アサーション（`!`）の制限

**説明**: 非 null アサーション演算子（`!`）を使用してはならない。null チェックまたはオプショナルチェーン（`?.`）で安全に処理する。

**このスタックで特に重要な理由**:
TypeScript 2.0 でリリースされた `!` 演算子は、コンパイラに対して「この値は null / undefined ではない」と伝えるが、ランタイムでの検証は一切行わない（参照: [TypeScript 2.0 Release Notes](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-2-0.html)）。`strict: true` によって `strictNullChecks` が有効化されているため、`!` を使用することはその保護を意図的に無効化することと等しい。React のフォームや DOM 参照で null が生じやすく、`!` の誤使用は NullPointerError 相当の実行時クラッシュを引き起こす。

**チェックルール**:
- YES: null / undefined の可能性がある値に対し、`if (value !== null)` / `value?.property` / `value ?? defaultValue` で安全に処理している。
- NO（違反）: `someValue!` または `someRef!.current` のように `!` を末尾に付けて null チェックを省略している。
- 例外: React の `useRef<HTMLElement>(null)` において、コンポーネントのマウント後であることが `useEffect` の実行順序により保証されている場合に限り、ref.current への `!` アクセスを許可する。その際はインラインコメントで理由が明示されていること。

---

## カテゴリ B: コンポーネント設計（React）

### P04: プレゼンテーション / ロジック分離

**説明**: `components/` 配下のコンポーネントは表示ロジックのみを持つ。副作用（`useEffect`）や API 通信（`useQuery` / `fetch`）を含むロジックは `hooks/` 配下のカスタムフックに分離する。副作用を伴わない単純な状態管理（`useState` のみ）はコンポーネント内に残してよい。

**このスタックで特に重要な理由**:
React の Hooks の導入により、ロジックの再利用はカスタムフックによって実現されるようになった。[React 公式ドキュメント — Using TypeScript](https://react.dev/learn/typescript) では、コンポーネントのロジックをフックとして抽出することが再利用性向上の手段として示されている。アーキテクチャ規約（`components/` はUIのみ、`hooks/` は状態・副作用・API 通信）はこの原則を明文化したものであり、副作用や API 通信のコンポーネントへの混入は規約の構造的矛盾となる。

**分離判定基準**:

| コンポーネント内のコード | 判定 |
|---|---|
| `useEffect` を含む | hooks へ分離必須 |
| `useQuery` / `fetch` 等の API 呼び出しを含む | hooks へ分離必須 |
| `useState` のみ（UI の開閉状態など） | コンポーネント内に残してよい |

**チェックルール**:
- YES: `components/` 配下のファイルが `useEffect` / `useQuery` / `fetch` 等の副作用・API 通信を直接呼び出していない。これらは `hooks/` 配下のカスタムフック経由でのみ提供される。`useState` のみの局所的な UI 状態管理はコンポーネント内で使用している。
- NO（違反）: `components/UserCard.tsx` の中で `useQuery` や `fetch` を直接呼び出している。または `useEffect` 内で API 通信やサブスクリプションを行っている。

---

### P05: props の型定義

**説明**: すべてのコンポーネントの props は `interface` または `type` で明示的に型定義し、暗黙的な `any` を持つ無名オブジェクト型を使用してはならない。

**このスタックで特に重要な理由**:
[React 公式ドキュメント — Using TypeScript](https://react.dev/learn/typescript) は「The type describing your component's props can be as simple or as complex as you need, though they should be an object type described with either a type or interface」と明示している。props の型定義は、コンポーネントの公開インターフェースを宣言する唯一の手段であり、型が省略されると `strict: true` の `noImplicitAny` で即座にコンパイルエラーになるか、明示的に `any` を書く必要が生じる。

**チェックルール**:
- YES: コンポーネント定義の直上に `interface XxxProps` または `type XxxProps = { ... }` が存在し、すべての props の型が明示されている。`children` を受け取る場合は `React.ReactNode` が指定されている。
- NO（違反）: `function MyComponent(props: any)` や、型定義なしで `{ title, onClick }` を受け取っている。
- 例外: なし。

---

### P06: コンポーネントの単一責任

**説明**: 1 つのコンポーネントが担う UI 責務は 1 つとする。複数の独立した UI 領域が含まれる場合、子コンポーネントに分割する。

**このスタックで特に重要な理由**:
React の設計思想は「components let you split the UI into independent, reusable pieces, and think about each piece in isolation」（[React 公式 — Thinking in React](https://react.dev/learn/thinking-in-react)）に基づく。shadcn/ui はコンポーネントの合成（Composition）を前提として設計されており、大きな単一コンポーネントへの詰め込みは shadcn/ui の設計意図に反する。

**レビュートリガー（参考基準）**: JSX のトップレベルのブロック要素が 4 個以上ある場合、レビュー時に分割の必要性を議論する。ただしこの数値は自動的な違反判定ではなく、レビューの起点とする。

**チェックルール**:
- YES: コンポーネントが単一の UI 責務を持っている。複数の独立した UI 領域がある場合は子コンポーネントに分割されている。
- NO（違反）: 1 ファイル内に「ヘッダー」「テーブル」「フッター」「モーダル」など、明らかに独立した UI 責務が直接 JSX として混在しており、分割されていない。
- 適用外: `pages/` 配下のページコンポーネントは子コンポーネントを配置する役割を持つため、本ルールの適用外とする。

---

## カテゴリ C: 非同期・副作用（React）

### P07: `useEffect` 内の非同期処理

**説明**: `useEffect` のコールバック関数を `async` にしてはならない。非同期処理は内部に定義した `async` 関数を呼び出す形で実装する。

**このスタックで特に重要な理由**:
[React 公式ドキュメント — useEffect](https://react.dev/reference/react/useEffect) は「The setup function should connect to an external system and return a cleanup function that disconnects from that system」と述べており、`useEffect` の戻り値はクリーンアップ関数（同期関数）でなければならない。`async` 関数は `Promise` を返すため、`useEffect` に直接 `async` を付けると、クリーンアップ関数が実行されなくなるバグが発生する。React 開発モードでは Strict Mode によって Effect が 2 回実行されるため（マウント→クリーンアップ→マウント）、この問題は開発環境でも顕在化する。

**チェックルール**:
- YES: `useEffect(() => { const fetch = async () => { ... }; fetch(); }, [deps])` のように、`useEffect` のコールバック自体は同期関数であり、内部で `async` 関数を定義・呼び出している。
- NO（違反）: `useEffect(async () => { await fetchData(); }, [])` のように `useEffect` のコールバック自体が `async` になっている。
- 例外: なし。

---

### P08: エラーハンドリング（Promise rejection の伝播）

**説明**: `Promise` の rejection および `async/await` の例外は必ず捕捉し、処理または呼び出し元に伝播させる。`.catch(() => {})` による握りつぶしは禁止する。

**このスタックで特に重要な理由**:
TanStack Query（React Query）は [TypeScript サポートドキュメント](https://tanstack.com/query/v4/docs/react/typescript) において、エラー型のデフォルトが `unknown` であることを明示しており、エラーを適切に型チェックした上で処理することを前提としている。バックエンドの FastAPI は `HTTPException` を通じて構造化されたエラーレスポンスを返すが、フロントエンド側でエラーを握りつぶすと、API エラー・ネットワーク障害・バリデーション失敗がすべて無音で失敗し、ユーザーと開発者の両方がデバッグ不能な状態に陥る。

**チェックルール**:
- YES: `try/catch` ブロックの `catch` 節に空ブロック `{}` が存在しない。`.catch()` ハンドラが少なくともエラーログ出力（`console.error` 等）またはエラー状態への格納を行っている。
- NO（違反）: `.catch(() => {})` または `catch (e) { /* 何もしない */ }` のように例外を無視している。
- 例外: キャンセルされたフェッチリクエスト（`AbortController` による `AbortError`）のように、特定の例外クラスを意図的に無視する場合は許可する。その際はインラインコメントで「AbortError のため無視」等の理由が明示されていること。

---

### P09: クリーンアップ関数

**説明**: `useEffect` 内でサブスクリプション・イベントリスナー・タイマー・フェッチリクエストを開始した場合は、必ずクリーンアップ関数を返して解除処理を実装する。

**このスタックで特に重要な理由**:
[React 公式ドキュメント — useEffect](https://react.dev/reference/react/useEffect) は「In development, React runs setup and cleanup one extra time before the setup as a stress-test. If this causes visible issues, your cleanup function is missing some logic」と明示している。クリーンアップを実装しない場合、コンポーネントのアンマウント後も非同期処理が継続し、アンマウント済みコンポーネントの状態を更新しようとするメモリリークや競合状態（race condition）が発生する。フェッチリクエストには `AbortController`、イベントリスナーには `removeEventListener` を使用する。

**チェックルール**:
- YES: `useEffect` 内で `addEventListener` / `setInterval` / `setTimeout` / `fetch` / WebSocket 接続を開始している場合、対応する `removeEventListener` / `clearInterval` / `clearTimeout` / `AbortController.abort()` / `close()` をクリーンアップ関数内で呼び出している。
- NO（違反）: `useEffect` 内で `addEventListener` を呼び出しているが、クリーンアップ関数（`return () => { ... }`）が存在しない。
- 例外: なし。

---

## カテゴリ D: API 通信（FE/BE 共通）

### P10: エラーレスポンスの形式統一

**説明**: API のエラーレスポンスは一貫したスキーマを持ち、フロントエンドでは discriminated union 型で処理する。

**このスタックで特に重要な理由**:
TanStack Query は [TypeScript ドキュメント](https://tanstack.com/query/v4/docs/react/typescript) で `useQuery` の結果が `status` フィールドによる discriminated union（`'pending' | 'error' | 'success'`）として定義されており、`status === 'error'` のときのみ `error` フィールドが非 null であることを型レベルで保証する。バックエンド（FastAPI）のエラーレスポンスも `{ "detail": "..." }` または `{ "code": "...", "message": "..." }` のように統一された構造である必要があり、フロントエンド側のエラー型定義と一致していなければ discriminated union の型絞り込みが機能しない。

**チェックルール**:
- YES: バックエンドのすべてのエラーレスポンスが `{ code: string; message: string }` の共通スキーマを持つ。フロントエンドの API クライアントが `type ApiResult<T> = { ok: true; data: T } | { ok: false; error: ApiError }` のような discriminated union で結果を表現している。
- NO（違反）: エンドポイントによってエラーレスポンスのフィールド名・構造が異なる（あるエンドポイントは `{ error: "..." }`、別のエンドポイントは `{ detail: "..." }` など）。
- 例外: FastAPI の標準バリデーションエラー（`422 Unprocessable Entity`、`{ "detail": [{ "loc": [...], "msg": "..." }] }` 形式）はフレームワークが自動生成するため、形式統一の対象外とする。ただしフロントエンドでの型定義には含めること。

---

### P11: HTTP ステータスコードの使い方

**説明**: FastAPI のエンドポイントは操作の結果に応じた適切な HTTP ステータスコードを返す。成功時の `200` 返却で代替してはならない。

**このスタックで特に重要な理由**:
FastAPI は `status_code` パラメータで正確なステータスコードを指定することを推奨しており、`fastapi.status` モジュールにすべての標準コードの定数が定義されている（参照: [FastAPI — Response Status Code](https://fastapi.tiangolo.com/tutorial/response-status-code/)）。REST API の設計においてステータスコードはセマンティクスの一部であり、フロントエンドの TanStack Query は `4xx` / `5xx` を自動でエラー扱いする前提で設計されている。`200` で包んだエラーレスポンスは自動エラーハンドリングを無効化する。

**チェックルール**:
- YES: 新規リソース作成は `201 Created`、削除成功は `204 No Content`、リソース未発見は `404 Not Found`、入力不正は `422 Unprocessable Entity`、認証失敗は `401 Unauthorized`、認可失敗は `403 Forbidden` を返している。
- NO（違反）: エラー情報を `{ success: false, message: "..." }` として `200 OK` で返している。または削除成功に `200 OK` と `{ deleted: true }` を返し `204` を使わない。
- 例外: 一部のクライアント（CORS 制約のある古いブラウザ等）がステータスコードを正しく扱えない場合、その旨をインラインコメントで明示した上で `200` 返却を許可する。

---

## カテゴリ E: レイヤー分離（FastAPI）

### P12: routers / services / repositories の責務分離

**説明**: `routers` はリクエスト受付・レスポンス整形のみを行い、ビジネスロジックを含んではならない。ビジネスロジックは `services` に、DB アクセスは `repositories` に集約する。`routers` から `repositories` を直接呼び出すことは禁止する。

**このスタックで特に重要な理由**:
FastAPI 公式ドキュメント [Dependencies](https://fastapi.tiangolo.com/tutorial/dependencies/) は「It is especially useful when you need to create layered logic. As you can see, dependencies can also have their own dependencies, which creates a hierarchical tree of dependencies」と述べ、責務をレイヤーに分けた依存注入を推奨する。`routers` が DB に直接アクセスすると、ビジネスルールがルーター全体に散在し、テスト・変更が困難になる。アーキテクチャ規約（`routers → services → repositories`）を厳守することで、この問題を構造的に防ぐ。

**チェックルール**:
- YES: `routers/` 配下のファイルが `from app.repositories import ...` を直接インポートしていない。`routers` は `services` のメソッドを呼び出し、`services` が `repositories` を呼び出す。
- NO（違反）: `routers/users.py` 内で `db.query(User).filter(...).all()` や `repository.find_by_id(...)` を直接呼び出している。
- 例外: なし。

---

### P13: 依存性注入（`Depends`）の使い方

**説明**: DB セッション・認証済みユーザー等の共通依存オブジェクトは `Depends()` を通じて注入し、ルーターハンドラ内で直接インスタンス化しない。

**このスタックで特に重要な理由**:
FastAPI 公式ドキュメント [Dependencies](https://fastapi.tiangolo.com/tutorial/dependencies/) は「you declare dependencies once and FastAPI provides them wherever needed, with no repetitive setup across routes」と述べている。`Depends()` を使わずに DB セッションや認証処理を各エンドポイントで直接実装すると、セッションのライフサイクル管理（`yield` によるクローズ）が保証されず、コネクションリークが発生する。また、テスト時に依存オブジェクトのモック差し替えができなくなる。

**チェックルール**:
- YES: DB セッション取得は `db: AsyncSession = Depends(get_db)` のように `Depends()` を通じて受け取っている。認証済みユーザーの取得も `current_user: User = Depends(get_current_user)` として注入されている。
- NO（違反）: ルーターハンドラ内で `db = SessionLocal()` のように直接セッションを生成している。または `token = request.headers.get("Authorization")` を各エンドポイントで個別に検証している。
- 例外: なし。

---

## カテゴリ F: バリデーション（Pydantic / FastAPI）

### P14: バリデーションはシステム境界（routers）で行う

**説明**: 外部入力（リクエストボディ・クエリパラメータ・パスパラメータ）のバリデーションはすべて `routers` 層で Pydantic スキーマまたは FastAPI の型アノテーションを通じて行い、`services` や `repositories` には検証済みデータのみを渡す。

**このスタックで特に重要な理由**:
[Pydantic v2 公式ドキュメント — Models](https://docs.pydantic.dev/latest/concepts/models/) は、モデルへの入力が Pydantic によって自動的に検証・変換されることを示している。FastAPI はルーターの型アノテーションから Pydantic バリデーションを自動実行するため、`routers` でスキーマを正しく定義するだけでシステム境界でのバリデーションが保証される。`services` 層でバリデーションを行うと、バリデーション責務が分散し、外部入力が生のまま DB に渡されるリスクが生じる。

**チェックルール**:
- YES: `routers/` のすべてのエンドポイントが、リクエストボディに Pydantic スキーマを型ヒントとして持ち（例: `body: UserCreateRequest`）、`services` のメソッドには型付きオブジェクトのみを渡している。
- NO（違反）: `request.body()` や `dict` として受け取ったデータを、Pydantic での検証なしに `services` や `repositories` に渡している。
- 例外: なし。

---

### P15: Request / Response Pydantic モデルの分離

**説明**: リクエスト用の Pydantic モデルとレスポンス用の Pydantic モデルは別クラスとして定義し、共有してはならない。

**このスタックで特に重要な理由**:
[Pydantic v2 公式ドキュメント](https://docs.pydantic.dev/latest/concepts/models/) はマルチモデルパターン（Base / Create / Update / Response）を示しており、Request と Response でモデルを共有した場合、Request にのみ存在すべきフィールド（パスワード等）がレスポンスに漏れる、またはレスポンスにのみ存在すべきフィールド（`id` / `created_at` 等）を Request で受け付けてしまうセキュリティ上・整合性上の問題が発生する。FastAPI は `response_model` パラメータでレスポンスの型を制御するが、モデルが共有されていると `response_model` の効果が薄れる。

**チェックルール**:
- YES: 同一リソースに対して `UserCreateRequest`（または `UserCreate`）と `UserResponse` が別クラスとして定義されている。`password` フィールドは `UserCreateRequest` にのみ存在し、`UserResponse` には存在しない。`id` / `created_at` 等のサーバー生成フィールドは `UserResponse` にのみ存在する。
- NO（違反）: `User` という単一の Pydantic クラスをリクエスト受付とレスポンス返却の両方に使い回している。
- 例外: 参照系のみのリソースでリクエストボディが存在しない場合（GET エンドポイントのみ）は Request モデルの定義は不要。

---

## カテゴリ G: DB・トランザクション（SQLAlchemy）

### P16: セッションの Unit of Work パターン

**説明**: DB セッションは `async with` ステートメントで管理し、操作の完了後に明示的に `commit()` を呼び出す。セッションのライフサイクルは 1 リクエストにつき 1 セッションとする。

**このスタックで特に重要な理由**:
[SQLAlchemy 2.0 公式ドキュメント — Session Basics](https://docs.sqlalchemy.org/en/20/orm/session_basics.html) は Unit of Work パターンについて「the Session first flushes all pending changes stored in memory to the database. This is known as the unit of work pattern」と定義している。セッションを明示的に管理せずに複数のリポジトリ操作に分散させると、部分的なコミットによるデータ不整合が発生する。FastAPI では `Depends(get_db)` と `yield` パターンを組み合わせてセッションのスコープを 1 リクエストに限定し、リクエスト完了時に自動クローズする。

**チェックルール**:
- YES: `get_db` 依存関数が `async with AsyncSession(...) as session: yield session` のパターンで実装されている。`repositories` の更新操作は `await session.commit()` を呼び出している。例外発生時は `await session.rollback()` が実行される（または `async with` が自動でロールバックする）。
- NO（違反）: セッションをグローバル変数として保持している。または `commit()` を呼び出さずにリクエスト終了時に自動コミットに依存している。
- 例外: なし。

---

### P17: N+1 クエリの回避

**説明**: 関連エンティティを取得する際は `joinedload()` または `selectinload()` を明示的に指定し、ループ内での追加クエリ発行（N+1 問題）を防ぐ。

**このスタックで特に重要な理由**:
[SQLAlchemy 2.0 公式ドキュメント — Relationship Loading Techniques](https://docs.sqlalchemy.org/en/20/orm/queryguide/relationships.html) は「selectinload() will ensure that a particular collection for a full series of objects are loaded up front using a single query」と述べ、N+1 問題を「the most common form of the N plus one problem」として明示的に取り上げている。SQLAlchemy 2.x では遅延ロードがデフォルトで無効（`lazy="raise"`）に設定できるが、遅延ロードを許可しつつ、明示的な eager load オプションを必要な箇所に指定する方針とする。

**チェックルール**:
- YES: 1 対多 / 多対多の関連を含むクエリには `.options(selectinload(Entity.relation))` または `.options(joinedload(Entity.relation))` が明示されている。コレクションには `selectinload`、単一オブジェクトの多対一には `joinedload` を使い分けている。
- NO（違反）: `for item in items: item.related_objects` のように、ループ内で関連オブジェクトへのアクセスが発生しており、eager load オプションが指定されていない。
- 例外: 関連オブジェクトのデータが不要であることが確実な場合（ID のみ使用など）は eager load を省略できる。その際はインラインコメントで「関連データ不使用のため eager load 省略」と明示されていること。

---

### P18: Alembic マイグレーションの運用ルール

**説明**: DB スキーマの変更は必ず Alembic マイグレーションファイルとして記録し、手動での DDL 実行を禁止する。自動生成（`--autogenerate`）後は必ず目視確認をしてからリポジトリにコミットする。

**このスタックで特に重要な理由**:
[Alembic 公式ドキュメント — Auto Generating Migrations](https://alembic.sqlalchemy.org/en/latest/autogenerate.html) は「autogenerate is not perfect — you should always review and edit the generated migration」と明記している。`--autogenerate` は列の追加・削除・型変更を検出するが、すべての変更（例: 制約名の変更、カスタムデータ型）を正確に検出できるわけではない。マイグレーションファイルをリポジトリで管理することで、チームメンバー間のスキーマ状態の差異を防ぎ、本番環境への適用手順を再現可能にする。

**チェックルール**:
- YES: モデルファイル（`models/`）への変更のたびに `alembic revision --autogenerate -m "変更内容"` が実行されている。生成されたマイグレーションファイルが `upgrade()` と `downgrade()` の両関数を持ち、`downgrade()` が空（`pass`）でない。マイグレーションファイルがリポジトリにコミットされている。
- NO（違反）: モデルの変更後にマイグレーションファイルが存在しない。または `downgrade()` が `pass` のみで実装されており、ロールバック不能な状態になっている。または直接 `psql` で DDL を実行してマイグレーションファイルと乖離している。
- 例外: 開発環境でのプロトタイピング中に限り、マイグレーションなしで DB をリセットすることを許可する。ただし main ブランチへのマージ前にはマイグレーションファイルが作成されていること。

---

## カテゴリ H: LLM 固有のアンチパターン

> **このカテゴリは人間のコードレビューでは見落とされやすいが、LLM が生成するコードで頻出するパターンを対象とする。**
> **すべて高重要度（C評価対象）とする。**

---

### P19: 型システム迂回パターン

**説明**: TypeScript の `as unknown as T`（ダブルキャスト）・`// @ts-ignore` / `// @ts-expect-error`、Python の `# type: ignore` / 引数型への `Any` 使用は型安全性を破壊するため禁止する。

**LLM がこれをやる理由**:
コンパイルエラー・型エラーを素早く解消するために型システムを迂回する。「動く」コードを出力するための近道として使われる。`as unknown as T` は P01（`any` 禁止）・P02（`as` 制限）の抜け穴として機能し、型検査をゼロにする。

**チェックルール**:
- YES: `as unknown as` パターンが存在しない。`// @ts-ignore` / `// @ts-expect-error` が存在しない。`# type: ignore` が存在しない。Python 関数の引数型に `Any` が使われていない。
- NO（違反）: `const user = data as unknown as User`（ダブルキャスト）。`// @ts-ignore` で型エラーを黙らせている。`def process(data: Any)` で引数型を消去している。
- 例外: サードパーティライブラリの型定義（`@types/` パッケージ含む）が存在しない場合に限り `// @ts-expect-error` を許可する。その際は直下のインラインコメントで「型定義なし: {ライブラリ名}」と理由が明示されていること。

---

### P20: スタブ・未完成実装

**説明**: 関数本体が空・`pass` のみ・`return []` / `return {}` / `return None` のみで構成される実装、および `TODO` / `FIXME` を唯一の内容とするコメントを本番コードに混入してはならない。

**LLM がこれをやる理由**:
タスクが複雑すぎる・コンテキストが不足している・時間制約があると感じた場合に、「動くように見えるが何もしない」スタブを生成する。特に `return []` や `return {}` は型チェックを通過してしまうため発見が遅れる。

**チェックルール**:
- YES: すべての関数・メソッドが実際のロジックを持つ。コメント中に `TODO:` / `FIXME:` が存在しない。
- NO（違反）: 関数本体が**唯一の文として** `pass` / `...` / `return []` / `return {}` / `return None` のみで構成されている（ロジックを伴わない固定値返却）。`# TODO: 後で実装` が唯一のコメント行。`raise NotImplementedError` が抽象基底クラス以外で使われている。TypeScript で `function save() { /* TODO */ }` のような未実装関数が存在する。
- 違反としないケース: ガード節としての早期リターン（例: `if not items: return []`）や、条件分岐の結果として空コレクションを返す場合はスタブではなく正常な実装とみなす。
- 例外: `abc.ABC` の抽象メソッドでの `raise NotImplementedError` は許可。TypeScript の `abstract` メソッドは本ルール適用外。

---

### P21: デバッグコード・一時コードの残置

**説明**: `console.log` / `console.debug` / `print()` / `pprint()` / `breakpoint()` などのデバッグ出力と、コメントアウトされたコードブロックを本番コードに残してはならない。

**LLM がこれをやる理由**:
実装・デバッグ過程で挿入した出力文をそのまま成果物として提出する。コメントアウトされたコードは「古い実装の参考のため残した」という判断で放置される。これらは機密情報（トークン・パスワード・個人情報）をログに出力するセキュリティリスクになりうる。

**チェックルール**:
- YES: `console.log` / `console.debug` / `console.info` が本番コードに存在しない。`print()` / `pprint()` / `breakpoint()` が存在しない。行全体がコメントになっているコードブロック（`// const old = ...` や `# result = ...` など）が存在しない。
- NO（違反）: `console.log('debug:', response)` が本番コードに混在。`# print(result)` のようなコメントアウトされたデバッグコードが残っている。`breakpoint()` がコード中に存在する。
- 例外: `console.error` / `console.warn` を構造化ログとして意図的に使用する場合は許可する。

---

### P22: SQLインジェクション防止

**説明**: SQLAlchemy の `db.execute()` に f-string・文字列連結・`%` フォーマットでユーザー入力を埋め込むことを禁止する。ORM メソッドまたは `bindparams()` を使用する。

**LLM がこれをやる理由**:
可読性・簡潔さを優先して生 SQL に f-string を使うコードを生成しやすい。OWASP Top 10 の A03（Injection）に直結するリスクであり、コードレビューで見逃されると本番環境でのSQLインジェクションに繋がる。SQLAlchemy の ORM 経由では発生しないが、`text()` を使う場合は手動でパラメータ化が必要。

**チェックルール**:
- YES: `db.execute()` を使用する場合は `text("SELECT ... WHERE id = :id").bindparams(id=user_id)` でパラメータ化している。または SQLAlchemy ORM の `.filter(Model.id == user_id)` を使用している。
- NO（違反）: `db.execute(f"SELECT * FROM users WHERE id = {user_id}")` のように f-string・文字列連結・`%` フォーマットでユーザー入力を SQL に埋め込んでいる。
- 例外: なし。

---

## 採点基準

### 採点方法

採点はファイル単位で行う。ファイル内のすべての原則（P01〜P22）を評価し、**最低評価の原則がそのファイルの総合グレード**となる。

### 評価グレード

| グレード | 定義 | アクション |
|---------|------|-----------|
| **S** | 全原則（P01〜P22）に準拠。模範的な実装 | — |
| **A** | 軽微な非準拠が 1〜2 件のみ（低重要度の原則のみ） | 修正推奨 |
| **B** | 中重要度の非準拠が 1 件、または低重要度の非準拠が 3 件以上 | 修正推奨 |
| **C2（Compliance）** | LLM アンチパターン違反。品質・保守性に関わる問題 | **マージ前に修正必須** |
| **C1（Critical）** | セキュリティ・データ整合性に関わる重大な違反 | **即座にレビュー中断・修正必須** |

### 原則の重要度分類

| 重要度 | 原則 | 評価 |
|--------|------|------|
| **最高（C1 対象）** | P08, P12, P14, P19, P22 | セキュリティ・データ整合性を直接脅かす違反 |
| **高（C2 対象）** | P01, P20, P21 | LLM が頻出させるアンチパターン。品質を損なう違反 |
| **中** | P02, P03, P07, P09, P10, P11, P13, P15, P16, P17, P18 | B 評価対象 |
| **低** | P04, P05, P06 | A 評価対象 |

### C1 評価（Critical）となる具体例

以下の違反が 1 件でも存在する場合、そのファイルは **C1 評価**となる。レビューを即座に中断し、修正を最優先する。

| 原則 | C1 評価となる違反の例 |
|------|-------------------|
| P08 | エラーを握りつぶしている（例: `.catch(() => {})` または `except: pass`） |
| P12 | `routers` から `repositories` を直接呼び出している |
| P14 | バリデーションなしで外部入力を直接 `services` / `repositories` に渡している |
| P19 | `as unknown as T` / `// @ts-ignore` / `# type: ignore` / 引数に `Any` |
| P22 | f-string / 文字列連結で SQL にユーザー入力を埋め込んでいる |

### C2 評価（Compliance）となる具体例

以下の違反が 1 件でも存在する場合、そのファイルは **C2 評価**となる。マージ前に修正が必要。

| 原則 | C2 評価となる違反の例 |
|------|-------------------|
| P01 | 型チェック回避のために `any` を使用している（例: `const data: any = response.json()`） |
| P20 | スタブ実装（関数本体が `pass` / `return []` のみ）や `TODO` のみのコメントが本番コードに存在する |
| P21 | `console.log` / `print()` / `breakpoint()` / コメントアウトコードが本番コードに存在する |

### チェックリスト（レビュー時の確認手順）

**ファイル種別で適用する原則が異なる。レビュー前に対象ファイルの種別を確認すること。**

| ファイル種別 | 適用原則 |
|------------|---------|
| `.ts` / `.tsx`（FE） | P01〜P11 |
| `.py`（BE） | P08（Python版）, P10〜P18 |
| 両方に共通 | P08（エラー握りつぶし）, P10（レスポンス形式）, P11（ステータスコード） |

#### FE ファイル（.ts / .tsx）のチェック順序

**C1 候補（最初に確認 — 該当あれば即中断）:**
1. `.catch(() => {})` / 空 `catch` が存在するか → P08 違反（C1）
2. `as unknown as T` / `// @ts-ignore` / `// @ts-expect-error` が存在するか → P19 違反（C1）

**C2 候補（次に確認 — マージ前に修正必須）:**
3. `any` が存在するか → P01 違反（C2）
4. 関数本体が固定値返却のみ / `TODO` のみの関数が存在するか → P20 違反（C2）
5. `console.log` / `console.debug` / コメントアウトコードが存在するか → P21 違反（C2）

**中重要度:**
6. unsafe narrowing（`as SomeType`）が型ガードなしに使われているか → P02 違反
7. `!` が型チェックなしに使われているか → P03 違反
8. `useEffect(async () => ...)` パターンが存在するか → P07 違反
9. `useEffect` 内でリソースを開いているが `return () => {...}` が存在しないか → P09 違反
10. エラーレスポンスの受け取りが discriminated union になっているか → P10 確認

**低重要度（最後に確認）:**
11. P04〜P06 の設計原則を確認

#### BE ファイル（.py）のチェック順序

**C1 候補（最初に確認 — 該当あれば即中断）:**
1. `except: pass` / 空 `except` が存在するか → P08 違反（C1）
2. `routers` ファイルが `repositories` を直接インポートしているか → P12 違反（C1）
3. `routers` が Pydantic スキーマを経由せず `dict` / 生データを `services` に渡しているか → P14 違反（C1）
4. `# type: ignore` / `Any` 型引数が存在するか → P19 違反（C1）
5. f-string / 文字列連結で SQL を構築しているか → P22 違反（C1）

**C2 候補（次に確認 — マージ前に修正必須）:**
6. `pass` のみ / `# TODO` のみの関数体が存在するか → P20 違反（C2）
7. `print()` / `pprint()` / `breakpoint()` / コメントアウトコードが存在するか → P21 違反（C2）

**中重要度:**
8. DB セッションを `Depends(get_db)` 経由で受け取っているか → P13 確認
9. Request / Response のモデルが分離されているか → P15 確認
10. セッションの `commit()` / `rollback()` が適切に実装されているか → P16 確認
11. 関連エンティティに `selectinload` / `joinedload` が指定されているか → P17 確認
12. モデル変更後にマイグレーションファイルが存在するか → P18 確認

---

## 例外一覧

各原則で許可されている例外を一覧にまとめる。例外を適用する際はインラインコメントで理由を明示すること。

| 原則 | 許可される例外 |
|------|-------------|
| P01 | サードパーティライブラリの型定義が存在しない場合に限り `any` を許可。`eslint-disable-next-line` + 理由コメント必須 |
| P02 | `as const`（リテラル型変換）、DOM API 操作（`e.target as HTMLInputElement` 等）、widening アサーションは許可 |
| P03 | `useRef<HTMLElement>(null)` で `useEffect` 実行順序によりマウント後が保証される場合に限り `!` を許可 |
| P04 | `useState` のみの局所的な UI 状態管理はコンポーネント内に残してよい（hooks 分離不要） |
| P06 | `pages/` 配下のページコンポーネントは適用外 |
| P08 | `AbortController` による `AbortError` を意図的に無視する場合は許可 |
| P10 | FastAPI 標準バリデーションエラー（422）はフレームワーク自動生成のため形式統一の対象外 |
| P11 | CORS 制約のある古いブラウザ対応で `200` 返却が必要な場合は許可 |
| P15 | GET エンドポイントのみのリソースは Request モデル不要 |
| P17 | 関連データが不要な場合（ID のみ使用など）は eager load 省略可 |
| P18 | 開発環境プロトタイピング中は DB リセット可。main マージ前にマイグレーション作成必須 |
| P19 | サードパーティの型定義が存在しない場合に限り `// @ts-expect-error` を許可。理由コメント必須 |
| P20 | `abc.ABC` の抽象メソッドでの `raise NotImplementedError` は許可。ガード節としての早期リターンはスタブとみなさない |
| P21 | `console.error` / `console.warn` を構造化ログとして意図的に使用する場合は許可 |
| P05, P07, P09, P12, P13, P14, P16, P22 | 例外なし |

---

## 補完ツール: Modern Web Guidance

本書はコードレビュー用の判定規約（型安全・設計・LLMアンチパターン）を定めるものであり、モダンWebプラットフォーム（最新のCSS・HTML・ブラウザAPI・パフォーマンス・アクセシビリティ・パスキー等）の実装知識はカバーしない。その領域は Google Chrome チーム公式の **Modern Web Guidance**（AIコーディングエージェント向けの専門家検証済みガイド集。102のWeb機能・128ユースケース、ブラウザ互換性データ付き）で補完する。

**役割分担**: 本書 = レビュー時の合否判定 / Modern Web Guidance = 実装時の知識注入（検索→取得）

### 使い方（CLI直接利用 — プラグイン不要）

フロントエンド実装Phaseで、対象機能の実装前に検索して該当ガイドをコンテキストに取り込む:

```bash
# ユースケースを検索（ローカルのセマンティック検索。ネットワーク・APIキー不要）
npx modern-web-guidance@latest search "実装したいことを英語で記述"

# ヒットしたIDでガイド本文（Markdown）を取得
npx modern-web-guidance@latest retrieve <ガイドID>
```

### Claude Code プラグインとして導入する場合

プラグインが利用できる環境では、以下で検索・取得が自動化される:

```
/plugin marketplace add GoogleChrome/modern-web-guidance
/plugin install modern-web-guidance@googlechrome
/reload-plugins
```

> **注意**: 2026年6月時点でプレビュー段階（コンテンツ拡充中）。詳細: https://developer.chrome.com/docs/modern-web-guidance

---

## 参考資料

本基準書の根拠とした公式ドキュメント:

- [TypeScript Handbook — Everyday Types](https://www.typescriptlang.org/docs/handbook/2/everyday-types.html)
- [TypeScript TSConfig Reference — strict](https://www.typescriptlang.org/tsconfig/strict.html)
- [TypeScript 2.0 Release Notes — Non-null assertion operator](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-2-0.html)
- [React 公式ドキュメント — useEffect](https://react.dev/reference/react/useEffect)
- [React 公式ドキュメント — Using TypeScript](https://react.dev/learn/typescript)
- [React 公式ドキュメント — Thinking in React](https://react.dev/learn/thinking-in-react)
- [TanStack Query — TypeScript](https://tanstack.com/query/v4/docs/react/typescript)
- [FastAPI — Dependencies](https://fastapi.tiangolo.com/tutorial/dependencies/)
- [FastAPI — Response Status Code](https://fastapi.tiangolo.com/tutorial/response-status-code/)
- [Pydantic v2 公式ドキュメント — Models](https://docs.pydantic.dev/latest/concepts/models/)
- [Pydantic v2 公式ドキュメント — Validators](https://docs.pydantic.dev/latest/concepts/validators/)
- [SQLAlchemy 2.0 — Session Basics](https://docs.sqlalchemy.org/en/20/orm/session_basics.html)
- [SQLAlchemy 2.0 — Relationship Loading Techniques](https://docs.sqlalchemy.org/en/20/orm/queryguide/relationships.html)
- [SQLAlchemy 2.0 — Asynchronous I/O](https://docs.sqlalchemy.org/en/20/orm/extensions/asyncio.html)
- [Alembic — Auto Generating Migrations](https://alembic.sqlalchemy.org/en/latest/autogenerate.html)
