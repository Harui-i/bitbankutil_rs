# 数値型

公開API・戦略・Bybit・約定モデルは`book_units`の専用整数型を使用します。
型間の暗黙変換、浮動小数点経由の入力、所定桁への丸めは行いません。

|型|内部整数|小数桁|用途|
|---|---|---:|---|
|`Price`|i64|8|価格、tick、スプレッド|
|`Quantity`|i64|8|板・注文・公開約定の数量、lot|
|`Rate`|i64|8|手数料率。負値はリベート|
|`Amount`|i128|24|売買代金、手数料、各資産の残高・拘束額|

```rust
use bitbankutil_rs::book_units::{Price, Quantity, Rate, Amount, NumericError};
let price: Price = "200.299".parse()?;
let quantity: Quantity = "10".parse()?;
let rate: Rate = "-0.0002".parse()?;
assert_eq!(price.notional(quantity)?, "2002.99".parse::<Amount>()?);
assert_eq!(price.fee(quantity, rate)?, "-0.400598".parse::<Amount>()?);
assert_eq!(quantity.to_amount().to_quantity()?, quantity);
# Ok::<(), NumericError>(())
```

`FromStr`は通常表記・指数表記を正確に読み取ります。末尾0を除いて8桁／24桁を超える小数は
`NumericError::Precision`、範囲超過は`Overflow`です。NaN・Infinity・空白付き数値は拒否します。
`from_units`は小数点を省いた内部整数から構築します。例えば`Price::from_units(1)`は0.00000001です。
`From<i32>`は整数の通貨・数量単位から安全に構築します。型間の変換には専用メソッドを使います。
8桁型の正の上限は92,233,720,368.54775807、Amountは約170兆です。

加減算・符号反転は`checked_add`／`checked_sub`／`checked_neg`を使います。
売買代金のraw値は`price_raw × quantity_raw × 10^8`、手数料は
`price_raw × quantity_raw × rate_raw`です。QuantityからAmountへの換算は`×10^16`です。
全て正確な整数演算で、途中の積を含むoverflowはエラーです。
`Amount::to_quantity`は余りと範囲を検証します。lotの余りは`Quantity::checked_remainder`で求めます。
資金計算ではf64を使わず、対数・グラフなどの近似分析に限り`to_f64`を明示して使用します。

JSON出力・取引所への発注値は正規化した十進文字列です。例：`"200.300"`は`"200.3"`、
`"0.00000001"`はそのまま保存します。外部JSONは数値・文字列の両方を受け付け、
`serde_json`の`arbitrary_precision`によりf64へ落とさず解析します。HTTPクエリの数値も正確な文字列で送信します。

従来の汎用十進型・float板の構築口・表示用map cacheは削除しています。
`Depth`と`MarketDepthSnapshot::new`は`BTreeMap<Price, Quantity>`を使用します。
`BybitDepth::update`は`Result`を返し、不正な更新は板へ一部だけ反映しません。
発注管理の`place_wanna_orders_concurrent`も数値エラーを返します。
旧ログと比較する場合は文字列表現の末尾0ではなく、数値として比較してください。
