# 開発ガイド

## フォーマット

```sh
cargo fmt --all --check
```

## テスト

通常の `cargo test` は、実際のbitbank APIへ接続せずに実行できます。純粋関数・変換・バリデーションに加え、paper tradingやローカルHTTPサーバーを使うテストを含みます。

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

## ドキュメント

ドキュメントは基本的に日本語で書く。

`README.md` は、bitbankutil_rsをほとんど知らない読者が、何ができるかを知るための入口とする。導入方法や機能をイメージできる短い例は載せても良い。
APIカバレッジのように、できることを把握するのに役立つ情報も載せてよい。詳しい使い方や動作の説明は `docs/` に書く。

`docs/` は、機能の使い方やbitbankutil_rs独自の概念を説明する場所とする。複数のAPIを組み合わせる手順、用途に応じた選択、全体の動作や制約を、利用者が目的を達成できるように説明する。

Rustソースコード中の個々の公開APIのドキュメントコメントは、そのAPIを使うためのリファレンスとする。APIの目的、引数、戻り値、エラー、動作上の制約や注意点など、呼び出し時に必要な情報を書く。
理解に役立つ短い使用例も含めてよい。複数APIにまたがる説明は `docs/` に置き、個々のAPIの説明からは必要に応じて参照する。

`docs/` のガイドは、`include_str!` を使って対応するモジュールのrustdocにも掲載してよい。この場合も `docs/` のファイルを正本とし、同じガイドをソースコードに重複して書かない。

実装や公開APIを変更するときは、変更した機能に関係するドキュメントとサンプルを確認し、必要に応じて同じ変更の中で更新する。コード例を追加・変更したときは、可能な範囲でコンパイルや実行を確認する。ドキュメントコメントの例は、実行できるものはdoctestとして書くことを推奨する。

### 用語

以下の用語は、ドキュメントの本文・見出し・例で原則として統一する。live tradingやpaper tradingに対する別の日本語表現は、複合語を含めて使わない。

live trading / paper trading / backtest は戦略の動かし方の呼び名である。背反 。

- live trading
リアルタイムのAPIにリクエストを送り、実際の取引所で現在注文の取得や新規注文の送信などをするような取引戦略の動かしかたのこと。Private APIもPublic APIも使う。 API key/secretが必要
「実取引」と呼ぶのはダサい。

- paper trading
取引戦略をシミュレーションする動かし方。リアルタイムのAPI情報などを参照するが、実際の取引所に現在注文の取得や新規注文の送信などを行わない取引のこと。
API Keyは不要。　注文の送信や約定の判定はシミュレートする。
「模擬取引」と呼ぶのはダサい

- backtest
取引戦略をシミュレーションする動かし方。過去のAPI情報などを記録したデータを参照して行う。
注文の送信や約定の判定に加えて、現在時刻などの情報もシミュレートする。 API Keyは不要だが過去のデータが必要
