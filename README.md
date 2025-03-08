[![GitHub Repo stars](https://img.shields.io/github/stars/Harui-i/bitbankutil_rs?style=social)](https://github.com/Harui-i/bitbankutil_rs)
[![dependency status](https://deps.rs/repo/github/Harui-i/bitbankutil_rs/status.svg)](https://deps.rs/repo/github/Harui-i/bitbankutil_rs)


# English
# bitbankutil_rs
`bitbankutil_rs` is a Rust library crate that provides multiple supports for handling bitbank API. By using `bitbankutil_rs`, you can easily achieve the following:

# Examples

## Using bitbank's Private API (e.g., retrieving current account balance)

By utilizing the `BitbankPrivateApiClient` defined in `src/bitbank_private.rs`, you can easily execute tasks as follows:

```rust
    let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
    let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();

    let bb_client = BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None)
    let assets = bb_client.get_assets().await;
    println!("{:?}", assets);
```

## Using bitbank's Public API (e.g., retrieving ticker information)

Using `BitbankPublicApiClient` defined in `src/bitbank_public.rs` makes implementation straightforward.

```rust
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_tickers().await;
    log::debug!("{:?}", res);
```

These operations require deserializing the exchange's JSON-formatted API responses into Rust objects using libraries like `serde_json`. Such processes are already implemented in `src/bitbank_structs.rs`, simplifying usage.

## Creating asynchronous event-driven bots for bitbank's WebSocket trades and order book updates

This can be achieved by using the `BotTrait` defined in `bitbank_bot.rs`.

`examples/best_mm.rs` is a sample code of an asynchronous event-driven bot that continuously places limit orders at the best price. To actually run it, use a command like
`cargo run --example best_mm mona_jpy 0.001 8000 0.001 0.002`. Here, the meanings of the arguments after `mona_jpy` are, as described in `examples/best_mm.rs`:
pair, tick size, order refresh interval (milliseconds), order size, and maximum position size.

# API Coverage

## Public API ([Doc](https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md))

| Endpoint                    | Status | 
| --------------------------- | ----------- | 
|`GET /{pair}/ticker`        | ✅         | 
|`GET /tickers`| ✅         |    
|`GET /tickers_jpy`|✅ |
|`GET /{pair}/depth`|✅ |
|`GET /{pair}/transactions/{YYYYMMDD}`|✅ |
|`GET /{pair}/candlestick/{candle-type}/{YYYY}`|❌ |
|`GET /{pair}/circut_break_info`|❌ |

## REST API ([Doc](https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md))

| Endpoint                    | Status | 
| --------------------------- | ----------- | 
|`GET /user/assets`|✅ |
|`GET /user/spot/order`|✅ |
|`POST /user/spot/order`|✅ |
|`POST /user/spot/cancel_order`|✅ |
|`POST /user/spot/cancel_orders`|✅ |
|`POST /user/spot/orders_info`|❌️ |
|`GET /user/spot/active_orders`|✅ |
|`GET /user/margin/positions`|❌️ |
|`GET /user/spot/trade_history`|✅ |
|`GET /user/deposit_history`|❌️ |
|`GET /user/unconfirmed_deposits`|❌️ |
|`GET /user/deposit_originators`|❌️ |
|`POST /user/confirm_deposits`|❌️ |
|`POST /user/confirm_deposits_all`|❌️ |
|`GET /user/withdrawal_account`|❌️ |
|`POST /user/request_withdrawal`|❌️ |
|`GET /user/withdrawal_history`|❌️ |
|`GET /spot/status`|✅ |
|`GET /spot/pairs`|❌️ |

# Usage

Add the following to the `[dependencies]` section of your `Cargo.toml` file:

```
bitbankutil_rs = {git = "https://github.com/Harui-i/bitbankutil_rs" }
```

# Contact

Twitter: [@Harui_botter](https://twitter.com/Harui_botter)

# Disclaimer

The developers of this project are not responsible for any losses incurred through the use of this library crate or sample programs. Use at your own risk.

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

`bitbank_bot.rs`で定義されている、`BotTrait`を利用することで、実現できます。

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
|`GET /{pair}/circut_break_info`|❌ |

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

