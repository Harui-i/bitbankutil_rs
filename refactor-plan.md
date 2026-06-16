# Refactor Plan

## 目的

paper walk-forward trading を後から実装しやすくするために、取引ロジックと bitbank API 依存部分を段階的に分離する。

この段階では `PaperExchange` / `PaperExecutionEngine` はまだ作らない。まずは live 実行の挙動を変えずに、注文計画・注文実行・市場データ変換の境界を作る。

## 方針

- 既存の live trading 挙動は変えない。
- 1ブランチごとにレビュー可能な小さい変更にする。
- 実APIなしで検証できる純粋ロジックのテストを増やす。
- paper trading 固有の約定モデル、仮想残高、レイテンシ、手数料は後続ブランチで扱う。

## 推奨ブランチ順

### 1. `refactor/order-domain-types` 完了

内部ロジック用の注文型を追加する。

候補:

- `OrderSide`
- `OrderType`
- `DesiredLimitOrder`
- `OpenOrder`
- `OrderId`
- `BalanceSnapshot`

このブランチでは、bitbank のレスポンス型を直接ロジックで扱う箇所を少しずつ内部型に変換する。`BitbankGetOrderResponse` などは API 境界の型として残し、内部ロジックには中立型を渡す方向に寄せる。

完了条件:

- bitbank レスポンスから内部注文型への変換関数がある。
- `side: String` や `type: String` を新規ロジックへ直接持ち込まない方針になっている。
- live の挙動は変わらない。

実施済み:

- `order_domain` に `OrderSide` / `OrderType` / `DesiredLimitOrder` / `OpenOrder` / `OrderId` / `BalanceSnapshot` を追加。
- `BitbankGetOrderResponse` から `OpenOrder` への変換を追加。
- `order_manager` と `examples/best_mm.rs` の注文判定を内部注文型へ寄せた。
- 内部注文型への変換テストを追加。

### 2. `refactor/order-planning`

`order_manager` から、注文差分計算を純粋関数として切り出す。

目標の形:

```text
current_orders + desired_orders + balances
  -> OrderPlan { cancels, placements }
```

このブランチでは、実際の発注・キャンセルはまだ `BitbankPrivateApiClient` を使ってよい。重要なのは「何をキャンセルし、何を新規発注するか」を API 呼び出しから分離すること。

完了条件:

- 注文計画を作る純粋関数がある。
- 主要ケースのユニットテストがある。
- 既存の `place_wanna_orders` / `place_wanna_orders_concurrent` の挙動が維持されている。

テストしたいケース:

- 既存注文と希望注文が一致している場合、何もしない。
- 希望注文にない既存注文はキャンセル対象になる。
- 未発注の希望注文は新規発注対象になる。
- 残高が足りる注文は先に発注できる。
- 残高が足りない注文はキャンセル後の発注対象になる。
- pair が違う注文は対象外になる。

実施済み:

- `order_manager` に `OrderPlan` と純粋関数 `plan_orders` を追加。
- bitbank レスポンスから `OpenOrder` への変換を async 実行関数の境界へ寄せた。
- `place_wanna_orders` / `place_wanna_orders_concurrent` が注文計画を経由するように変更。
- 既存注文一致、キャンセル、新規発注、残高による先行/後続発注、別 pair 除外のユニットテストを追加。

### 3. `refactor/order-executor-trait`

注文実行を trait 経由にする。

候補:

```rust
trait OrderExecutor {
    async fn place_order(&self, order: PlacementRequest) -> Result<PlacedOrder, OrderError>;
    async fn cancel_orders(&self, pair: &str, order_ids: Vec<OrderId>) -> Result<(), OrderError>;
}
```

このブランチでは live 用の `BitbankOrderExecutor` だけ実装する。paper 用実装はまだ不要。テストでは `FakeOrderExecutor` を使って、計画通りに呼び出されることを確認する。

完了条件:

- `order_manager` が `BitbankPrivateApiClient` に直接依存しない。
- live 用 executor が既存の bitbank private API を呼ぶ。
- fake executor を使ったテストがある。
- live の挙動は変わらない。

実施済み:

- `order_executor` に `OrderExecutor` / `PlacementRequest` / `PlacedOrder` / `OrderExecutionError` を追加。
- live 用の `BitbankOrderExecutor` を追加し、既存の bitbank private API 呼び出しへ委譲。
- 既存呼び出し互換のため `BitbankPrivateApiClient` 自体にも `OrderExecutor` を実装。
- `order_manager` の発注・キャンセル実行を `OrderExecutor` 経由に変更。
- fake executor を使い、通常実行と concurrent 実行が計画通りに呼び出されるユニットテストを追加。

### 4. `refactor/market-event-boundary`

市場データも bitbank 固有型から中立イベントへ変換できる境界を作る。

現在の `BitbankEvent` / `forward_bitbank_messages` は、リアルタイム feed とログ再生を同じ流れに載せやすい良い土台になっている。このブランチでは必要に応じて `MarketEvent` / `MarketSnapshot` のような内部型を追加する。

完了条件:

- 戦略や将来の paper engine が bitbank 固有レスポンスに強く依存しない。
- WebSocket 由来イベントとログ再生イベントを同じ中立イベント列として扱える。
- 既存の bot runtime の挙動は変わらない。

### 5. `feature/paper-execution-engine`

ここで初めて paper trading 固有の実装を入れる。

扱う候補:

- 仮想残高
- 仮想注文帳
- 注文受付・キャンセル・約定イベント
- limit / market 約定判定
- 部分約定
- 手数料
- レイテンシ
- walk-forward 用のイベント再生

前段までで `OrderExecutor` と `MarketEvent` の境界ができていれば、live と paper の切り替えは executor 差し替えとして実装できる。

## 注意点

- paper 実装を急いで入れない。先に境界を作る。
- `order_manager` に live/paper の分岐を直接増やさない。
- bitbank API レスポンス型を内部ドメインモデルとして使い続けない。
- 文字列ベースの `side` / `type` 分岐は新規コードでは避ける。
- 実APIに依存するテストと、実APIなしで動くユニットテストを分ける。

## 最終的な狙い

最終的には次の形に寄せる。

```text
MarketDataSource
  -> MarketEvent
  -> Strategy
  -> DesiredLimitOrder
  -> OrderPlanner
  -> OrderPlan
  -> OrderExecutor
       - BitbankOrderExecutor
       - FakeOrderExecutor
       - PaperOrderExecutor
```

この形にしておけば、リアルタイムの取引所データを使いつつ、実注文は出さずに paper execution へ流す構成を自然に作れる。
