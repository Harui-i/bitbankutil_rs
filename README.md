# bitbankutil_rs

[![GitHub Repo stars](https://img.shields.io/github/stars/Harui-i/bitbankutil_rs?style=social)](https://github.com/Harui-i/bitbankutil_rs)
[![dependency status](https://deps.rs/repo/github/Harui-i/bitbankutil_rs/status.svg)](https://deps.rs/repo/github/Harui-i/bitbankutil_rs)
[![crates.io](https://img.shields.io/crates/v/bitbankutil_rs)](https://crates.io/crates/bitbankutil_rs)
[![docs.rs](https://img.shields.io/docsrs/bitbankutil_rs)](https://docs.rs/bitbankutil_rs)

`bitbankutil_rs`は、RustでbitbankのAPI処理を複数サポートしたライブラリクレートです。 `bitbankutil_rs`を使うことで、以下のようなことが簡単に実現できます。

価格・数量・率・残高は専用の固定小数点整数型で扱います。型と精度・移行方法は[数値型ガイド](docs/numeric-types.md)を参照してください。

運用ログの設定とイベント一覧は[ログガイド](docs/logging.md)を参照してください。ライブラリはsubscriberを初期化せず、呼び出し側が出力先・形式・フィルターを設定します。

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
    tracing::debug!("{:?}", res);
```

取引所ステータスと取引ペア設定も、同じクライアントで API キーなしで取得できます。
`get_status` の呼び出しは `BitbankPublicApiClient` に移行してください。

```rust
    let statuses = public_client.get_status().await.unwrap().statuses;
    let pairs = public_client.get_pairs().await.unwrap().pairs;
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
cargo run --example best_mm -- --paper mona_jpy 0.001 8000 0.001 0.002 0 10000 10000
```

引数は、ペア、ティックサイズ、注文更新間隔（ミリ秒）、注文数量、最大保有数量です。
`--paper`では対象通貨とJPYの初期残高、内部再構成のための待機時間（ミリ秒）も指定します。
ライブラリの既定待機時間は10秒です。Paperは実時間からその分遅れた仮想世界を動かし、
内部市場は発生時刻、戦略への情報は元の受信時刻で処理します。バックテストも同じ実行基盤を使います。
待機時間内に必要な市場情報が届くという仮定であり、欠損や遅延上限超過では執行を停止します。
注文・照会・通知の遅延は別に設定できます。共有FIFOによる推定で、真のキュー順位を保証しません。
[約定モデルと時計](docs/paper-execution.md)、[バックテスト](docs/offline-replay.md)を参照してください。
実行記録は既定で資金・注文の監査情報だけを保存します。キューの全件・周期snapshot・差分・counterを選ぶ方法は
[実行記録](docs/execution-recording.md#記録量を選ぶ)を参照してください。

発注・取消から時計差の区間を求め、WSの到着遅延を分析するための任意の観測ログも利用できます。
[時計観測ガイド](docs/clock-observation.md)を参照してください。

## API カバレッジ

### Public API ([公式仕様](https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api_JP.md))

| エンドポイント | 実装状況 |
| --- | --- |
| `GET /{pair}/ticker` | ✅ |
| `GET /tickers` | ✅ |
| `GET /tickers_jpy` | ✅ |
| `GET /{pair}/depth` | ✅ |
| `GET /{pair}/transactions/{YYYYMMDD}` | ✅ 日付は省略可能。省略時は最新60件 |
| `GET /{pair}/candlestick/{candle-type}/{date}` | ✅ `1min`、`5min`、`15min`、`30min`、`1hour` は `YYYYMMDD`、`4hour`、`8hour`、`12hour`、`1day`、`1week`、`1month` は `YYYY` を指定 |
| `GET /{pair}/circuit_break_info` | ✅ |

### REST API ([公式仕様](https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api_JP.md))

| エンドポイント | 実装状況 |
| --- | --- |
| `GET /user/assets` | ✅ |
| `GET /user/spot/order` | ✅ 発動時刻 `triggered_at` は未対応 ([#93](https://github.com/Harui-i/bitbankutil_rs/issues/93)) |
| `POST /user/spot/order` | ✅ 現物の指値・成行・逆指値・逆指値指値に対応。信用発注は未対応 ([#95](https://github.com/Harui-i/bitbankutil_rs/issues/95)) |
| `POST /user/spot/cancel_order` | ✅ |
| `POST /user/spot/cancel_orders` | ✅ |
| `POST /user/spot/orders_info` | ❌ 未実装 ([#92](https://github.com/Harui-i/bitbankutil_rs/issues/92)) |
| `GET /user/spot/active_orders` | ✅ 単一注文照会と同様に `triggered_at` は未対応 ([#93](https://github.com/Harui-i/bitbankutil_rs/issues/93)) |
| `GET /user/margin/status` | ❌ 未実装 ([#94](https://github.com/Harui-i/bitbankutil_rs/issues/94)) |
| `GET /user/margin/positions` | ❌ 未実装 ([#94](https://github.com/Harui-i/bitbankutil_rs/issues/94)) |
| `GET /user/spot/trade_history` | ✅ |
| `GET /user/deposit_history` | ❌ 未実装 ([#97](https://github.com/Harui-i/bitbankutil_rs/issues/97)) |
| `GET /user/unconfirmed_deposits` | ❌ 未実装 ([#97](https://github.com/Harui-i/bitbankutil_rs/issues/97)) |
| `GET /user/deposit_originators` | ❌ 未実装 ([#97](https://github.com/Harui-i/bitbankutil_rs/issues/97)) |
| `POST /user/confirm_deposits` | ❌ 未実装 ([#97](https://github.com/Harui-i/bitbankutil_rs/issues/97)) |
| `POST /user/confirm_deposits_all` | ❌ 未実装 ([#97](https://github.com/Harui-i/bitbankutil_rs/issues/97)) |
| `GET /user/withdrawal_account` | ❌ 未実装 ([#98](https://github.com/Harui-i/bitbankutil_rs/issues/98)) |
| `POST /user/request_withdrawal` | ❌ 未実装 ([#98](https://github.com/Harui-i/bitbankutil_rs/issues/98)) |
| `GET /user/withdrawal_history` | ❌ 未実装 ([#98](https://github.com/Harui-i/bitbankutil_rs/issues/98)) |
| `GET /spot/status` | ✅ `BitbankPublicApiClient::get_status` で API キーなしで取得可能 |
| `GET /spot/pairs` | ✅ `BitbankPublicApiClient::get_pairs` で API キーなしで取得可能 |
| `GET /user/subscribe` | ✅ `get_channel_and_token` で PubNub のチャンネル名とトークンを取得可能 |

単一注文照会とアクティブ注文照会のレスポンスに `triggered_at` が含まれる場合、通常設定ではこの項目を読み飛ばし、`strict-validation` ではデシリアライズに失敗します。

### Public Stream API ([公式仕様](https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-stream_JP.md))

| チャンネル | 実装状況 |
| --- | --- |
| `ticker_{pair}` | ✅ |
| `transactions_{pair}` | ✅ |
| `depth_diff_{pair}` | ✅ |
| `depth_whole_{pair}` | ✅ |
| `circuit_break_info_{pair}` | ✅ |

### Private Stream API ([公式仕様](https://github.com/bitbankinc/bitbank-api-docs/blob/master/private-stream_JP.md))

| 機能 | 実装状況 |
| --- | --- |
| PubNub での購読・通知受信 | ❌ 未実装 ([#99](https://github.com/Harui-i/bitbankutil_rs/issues/99)) |

## 使い方

プロジェクトのディレクトリで以下を実行してください。

```sh
cargo add bitbankutil_rs
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
