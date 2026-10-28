# bitbankutil_rs

[![GitHub Repo stars](https://img.shields.io/github/stars/Harui-i/bitbankutil_rs?style=social)](https://github.com/Harui-i/bitbankutil_rs)
[![dependency status](https://deps.rs/repo/github/Harui-i/bitbankutil_rs/status.svg)](https://deps.rs/repo/github/Harui-i/bitbankutil_rs)

`bitbankutil_rs`は、RustでbitbankのAPI処理を複数サポートしたライブラリクレートです。 `bitbankutil_rs`を使うことで、以下のようなことが簡単に実現できます。

詳しい利用方法は [APIドキュメント](https://docs.rs/bitbankutil_rs) を参照してください。

## Examples

### bitbankのPrivate APIを利用した処理(現在の残高の取得など)

`src/bitbank_private.rs`で定義されている、`BitbankPrivateApiClient`を使用することで、次のように簡単に実行することができます。

```rust
    let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
    let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();

    let bb_client = BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None);
    let assets = bb_client.get_assets().await;
    println!("{:?}", assets);
```

### bitbankのPublic APIを利用した処理(ティッカーの取得など)

`src/bitbank_public.rs`で定義されている`BitbankPublicApiClient`を使用することで、簡単に実装できます。

```rust
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_tickers().await;
    log::debug!("{:?}", res);
```

以上のような処理には、取引所のjson形式のAPIレスポンスをRustで扱えるようにするために`serde_json`などを使ってデシリアライズする必要がありますが、そういった処理は`src/bitbank_structs.rs`で実装されているため、簡単に利用できます。

### bitbankのWebSocketでの約定や板情報の更新などに応じた非同期イベント駆動botの制作

取引を行うbotは [`examples/best_mm.rs`](examples/best_mm.rs)、
取引せず板情報を表示するプログラムは [`examples/board_viewer.rs`](examples/board_viewer.rs) を参照してください。

`best_mm` は最良価格に指値注文を出し続けるサンプルです。起動時にlive tradingかpaper trading（paper）を指定できます。

```sh
# live trading（BITBANK_API_KEY / BITBANK_API_SECRET が必要）
cargo run --example best_mm -- --live mona_jpy 0.001 8000 0.001 0.002

# paper trading（APIキー不要。公開WebSocketのデータを使い、注文・残高はメモリ上で管理）
cargo run --example best_mm -- --paper mona_jpy 0.001 8000 0.001 0.002 0 10000
```

引数は、ペア、ティックサイズ、注文更新間隔（ミリ秒）、注文数量、最大保有数量です。
`--paper`を指定する場合は対象通貨,初期残高も指定する必要があります。注文キュー内の順位やレイテンシは再現しません。

## API カバレッジ

### Public API ([Doc](https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md))

| エンドポイント                    | 実装状況 |
| --------------------------- | ----------- |
|`GET /{pair}/ticker`        | ✅         |
|`GET /tickers`| ✅         |
|`GET /tickers_jpy`|✅ |
|`GET /{pair}/depth`|✅ |
|`GET /{pair}/transactions/{YYYYMMDD}`|✅ |
|`GET /{pair}/candlestick/{candle-type}/{YYYY}`|✅ |
|`GET /{pair}/circuit_break_info`| ✅ |

### REST API ([Doc](https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md))

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

### Public Stream API

|チャンネル | 実装状況 |
| --------------------------- | ----------- |
|`ticker_{pair}`|✅ |
|`transactions_{pair}`|✅ |
|`depth_diff_{pair}`|✅ |
|`depth_whole_{pair}`|✅ |
|`circuit_break_info_{pair}`|✅ |

## 今後の実装予定

- Private Streaming APIへの対応

## 使い方

`Cargo.toml`の`[dependencies]`に以下を追加してください。

```toml
[dependencies]
bitbankutil_rs = "0.1.9"
```

API通信には、bitbank対応を追加した`crypto-botters`のforkである
[`crypto-botters-bitbank`](https://crates.io/crates/crypto-botters-bitbank)を使用しています。

開発・テスト手順は [開発ガイド](CONTRIBUTING.md) を参照してください。

## ライセンス

本プロジェクトはMITまたはApache-2.0のいずれかを選択して利用できます。

## 連絡先

Twitter: [@Harui_botter](https://twitter.com/Harui_botter)

## 注意

このライブラリクレートやサンプルプログラムの利用によって生じたいかなる損失についても、当プロジェクトの開発者は責任を負いかねます。自己責任でご利用ください。
