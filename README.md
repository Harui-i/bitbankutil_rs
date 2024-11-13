# bitbankutil_rs
`bitbankutil_rs`は、RustでbitbankのAPI処理を複数サポートしたライブラリクレートです。 `bitbankutil_rs`を使うことで、以下のようなことが簡単に実現できます。

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

# 連絡先

Twitter: [@Harui_botter](https://twitter.com/Harui_botter)

# 注意

このライブラリクレートやサンプルプログラムの利用によって生じたいかなる損失についても、当プロジェクトの開発者は責任を負いかねます。自己責任でご利用ください。