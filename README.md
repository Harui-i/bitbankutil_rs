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

`bitbank_bot.rs`で定義されている`BitbankBotBuilder`と`BotStrategy`を利用すると、
WebSocketイベントを扱う際に状態を自前で受け渡す必要がなくなり、取引ロジックに
集中できます。`BotContext::event_sender`を使えば、ログのリプレイや他取引所の
情報などユーザー独自のデータソースも同じランタイムに流し込めます。

`examples/best_mm.rs`は非同期イベント駆動で、best価格に指値注文をし続けるbotのサンプルコードです。実際に実行するには
`cargo run --example best_mm mona_jpy 0.001 8000 0.001 0.002` のようにしてください。ここで、`mona_jpy`以降の引数の意味は、`examples/best_mm.rs`に書いてあるとおり、
ペア,ティックサイズ(呼び値)、 注文を入れ替える感覚(ミリ秒)、 一回の注文のサイズ、 最大保有数となっています。


# API カバレッジ

## Public API ([Doc](https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md))

| エンドポイント                    | 実装状況 | 
| --------------------------- | ----------- | 
|`GET /{pair}/ticker`        | ✅         | 
|`GET /tickers`| ✅         |    
|`GET /tickers_jpy`|✅ |
|`GET /{pair}/depth`|✅ |
|`GET /{pair}/transactions/{YYYYMMDD}`|✅ |
|`GET /{pair}/candlestick/{candle-type}/{YYYY}`|❌ |
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
