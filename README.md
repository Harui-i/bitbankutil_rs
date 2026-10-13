[![GitHub Repo stars](https://img.shields.io/github/stars/Harui-i/bitbankutil_rs?style=social)](https://github.com/Harui-i/bitbankutil_rs)
[![dependency status](https://deps.rs/repo/github/Harui-i/bitbankutil_rs/status.svg)](https://deps.rs/repo/github/Harui-i/bitbankutil_rs)



# 日本語
# bitbankutil_rs
`bitbankutil_rs`は、RustでbitbankのAPI処理を複数サポートしたライブラリクレートです。 `bitbankutil_rs`を使うことで、以下のようなことが簡単に実現できます。

# Examples

## bitbankのPrivate APIを利用した処理(現在の残高の取得など)

`src/bitbank_private.rs`で定義されている、`BitbankPrivateApiClient`を使用することで、次のように簡単に実行することができます。

```rust
    let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
    let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();

    let bb_client = BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None)
    let assets = bb_client.get_assets().await;
    println!("{:?}", assets);
```

## bitbankのPublic APIを利用した処理(ティッカーの取得など)

`src/bitbank_public.rs`で定義されている`BitbankPublicApiClient`を使用することで、簡単に実装できます。

```rust
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_tickers().await;
    log::debug!("{:?}", res);
```


以上のような処理には、取引所のjson形式のAPIレスポンスをRustで扱えるようにするために`serde_json`などを使ってデシリアライズする必要がありますが、そういった処理は`src/bitbank_structs.rs`で実装されているため、簡単に利用できます。

## bitbankのWebSocketでの約定や板情報の更新などに応じた非同期イベント駆動botの制作

`BitbankBotBuilder` と `BotStrategy` を利用すると、戦略は市場データに応じた判断と注文操作に集中できます。
取引を行う場合は `BitbankBotBuilder::new(strategy, venue)` で取引先を一度だけ指定します。
戦略は `ctx.venue()` 経由で残高取得・発注・取消を行い、取引先を自身の設定に保持する必要はありません。
板表示など注文機能が不要な場合は `BitbankBotBuilder::market_data(strategy)` を使います。
この構成での口座取得・注文操作は `TradingVenueError::Unavailable` を返します。

戦略は `BotStrategy<V>` を実装し、関連型 `CustomEvent` に独自イベント型を指定します。
独自イベントを使わない場合は `std::convert::Infallible` を指定します。
`handle_event()` は `BotEvent::Market(MarketEvent)` または `BotEvent::Custom(CustomEvent)` を受け取ります。

```rust,ignore
impl<V: TradingVenue> BotStrategy<V> for MyBot {
    type CustomEvent = std::convert::Infallible;

    async fn handle_event(
        &mut self,
        event: BotEvent<Self::CustomEvent>,
        ctx: &BotContext<Self::CustomEvent, V>,
    ) {
        if let BotEvent::Market(MarketEvent::Transactions { pair, .. }) = event {
            let account = ctx.venue().account_snapshot(&pair).await;
            // 更新済みの口座状態を使って判断する
        }
    }
}

let runtime = BitbankBotBuilder::new(strategy, venue)
    .add_pair("btc_jpy")
    .spawn();
```

市場イベントは、ランタイムが受信順に `TradingVenue::on_market_event()` へ反映してから戦略へ渡します。
paperでは仮想約定・残高更新を先に行い、liveではこの通知自体は何もしません。
戦略から市場イベント反映を手動で呼ぶ必要はありません。
反映に失敗した場合はエラーを記録し、そのイベントの戦略実行をスキップします。
独自イベントは取引先への反映を行わず、戦略へ渡します。

`BotContext::event_sender()` とランタイムの `event_sender()` は `Sender<BotEvent<C>>` を返し、
`ctx.emit(BotEvent::Custom(...))` などで独自イベントを送れます。
市場イベントの手動送信には `BotEvent::Market(...)` を使います。
`forward_bitbank_messages()` にも同じ送信者を渡せるため、ログ再生も同じ順序で処理されます。
取引先のクローンは同じ口座・注文状態を共有する必要があります。

旧APIからの移行では、`BitbankEvent` を `MarketEvent` と `BotEvent::Market` に置き換え、
`BotStrategy::Event` を `CustomEvent` に変更します。`observe_market_event()` は削除しています。

`examples/best_mm.rs`は非同期イベント駆動で、best価格に指値注文をし続けるbotのサンプルコードです。起動時に取引モードを明示します。

```sh
# 実注文を出す。BITBANK_API_KEY / BITBANK_API_SECRET が必要
cargo run --example best_mm -- --live mona_jpy 0.001 8000 0.001 0.002

# 公開WebSocketの実データを使い、注文・残高はメモリ上で模擬する。APIキーは不要
cargo run --example best_mm -- --paper mona_jpy 0.001 8000 0.001 0.002 0 10000
```

共通の引数は、ペア、ティックサイズ、注文更新間隔（ミリ秒）、一回の注文数量、最大保有数量です。paper モードではさらに対象通貨と JPY の初期残高を指定します。初期残高は非負で、paper モードは JPY ペアに対応します。状態は保存されず、再起動時は指定した初期残高から始まります。

paper の約定は公開 WebSocket の取引イベントを使う既存の maker モデルで判定します。部分約定と手数料は反映しますが、発注時の板との交差、注文キュー内の順位、レイテンシは再現しません。注文受付・取消・約定・拒否はログに出力します。


# API カバレッジ

## Public API ([Doc](https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md))

| エンドポイント                    | 実装状況 | 
| --------------------------- | ----------- | 
|`GET /{pair}/ticker`        | ✅         | 
|`GET /tickers`| ✅         |    
|`GET /tickers_jpy`|✅ |
|`GET /{pair}/depth`|✅ |
|`GET /{pair}/transactions/{YYYYMMDD}`|✅ |
|`GET /{pair}/candlestick/{candle-type}/{YYYY}`|✅ |
|`GET /{pair}/circuit_break_info`| ✅ |

## REST API ([Doc](https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md))

| エンドポイント                    | 実装状況 | 
| --------------------------- | ----------- | 
|`GET /user/assets`|✅ |
|`GET /user/spot/order`|✅ |
|`POST /user/spot/order`|✅ |
|`POST /user/spot/cancel_order`|✅ |
|`POST /user/spot/cancel_orders`|✅ |
|`POST /user/spot/orders_info`|❌️ |
|`GET /user/spot/active_orders`|✅|
|`GET /user/margin/positions`|❌️ |
|`GET /user/spot/trade_history`|✅|
|`GET /user/deposit_history`|❌️ |
|`GET /user/unconfirmed_deposits`|❌️ |
|`GET /user/deposit_originators`|❌️ |
|`POST /user/confirm_deposits`|❌️ |
|`POST /user/confirm_deposits_all`|❌️ |
|`GET /user/withdrawal_account`|❌️ |
|`POST /user/request_withdrawal`|❌️ |
|`GET /user/withdrawal_history`|❌️ |
|`GET /spot/status`|️✅ |
|`GET /spot/pairs`|❌️ |
|`GET /user/subscribe`| ✅ |

## Public Stream API

|チャンネル | 実装状況 | 
| --------------------------- | ----------- | 
|`ticker_{pair}`|✅ |
|`transactions_{pair}`|✅ |
|`depth_diff_{pair}`|✅ |
|`depth_whole_{pair}`|✅ |
|`circuit_break_info_{pair}`|✅ |


# 今後の実装予定

- Private Streaming APIへの対応

# テスト

通常の `cargo test` は、実際のbitbank APIを叩かない純粋関数・変換・バリデーションのテストだけを実行します。

実際のPublic APIを叩くテストを実行する場合:

```sh
cargo test --features live-public-api --test live_public_api
```

実際のPrivate APIを叩く読み取り系テストを実行する場合:

```sh
cargo test --features live-private-api --test live_private_api -- --test-threads=1
```

実注文・キャンセルを伴うPrivate APIテストを実行する場合:

```sh
BITBANKUTIL_RUN_PRIVATE_ORDER_TESTS=1 cargo test --features live-private-order-api --test live_private_order_api -- --test-threads=1
```

# 使い方

`Cargo.toml`の`[dependencies]`の欄に
```
bitbankutil_rs = {git = "https://github.com/Harui-i/bitbankutil_rs" }
```
などと追加してください。

# 連絡先

Twitter: [@Harui_botter](https://twitter.com/Harui_botter)

# 注意

このライブラリクレートやサンプルプログラムの利用によって生じたいかなる損失についても、当プロジェクトの開発者は責任を負いかねます。自己責任でご利用ください。
