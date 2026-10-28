# 開発ガイド

## フォーマット

```sh
cargo fmt --all --check
```

## テスト

通常の `cargo test` は、実際のbitbank APIへ接続せずに実行できます。純粋関数・変換・バリデーションに加え、模擬取引やローカルHTTPサーバーを使うテストを含みます。

CIではサンプル内のテストも含め、通常設定と `strict-validation` 有効時の両方を実行します。ローカルでも同じ確認を行えます:

```sh
cargo test --locked --all-targets
cargo test --locked --all-targets --features strict-validation
```

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
