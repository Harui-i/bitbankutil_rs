# bitbankutil_rs
In addition to structures representing bitbank's (Pubilc/Private/WebSocket) API responses and board information, t also provides BotTrait, which is useful for creating simple event-driven bots.



## サンプルコード
`examples/best_mm.rs`は、best価格に指値注文をし続けるbotのサンプルコードです。実際に実行するには

`cargo run --example best_mm mona_jpy 0.001 8000 0.001 0.002` のようにしてください。ここで、`mona_jpy`以降の引数の意味は、`examples/best_mm.rs`に書いてあるとおり、
ペア,ティックサイズ(呼び値)、 注文を入れ替える感覚(ミリ秒)、 一回の注文のサイズ、 最大保有数となっています。