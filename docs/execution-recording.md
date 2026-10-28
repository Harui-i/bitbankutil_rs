# 実行記録

`execution_recording` は、botが行った取引操作とその結果を保存し、実行後に調べるためのモジュールです。
たとえば「発注が失敗した理由を調べたい」「APIを何回呼んだか数えたい」
「paper tradingで注文や残高がどう変わったか確認したい」ときに使います。

live tradingとpaper tradingのどちらでも利用できます。記録には、口座情報の取得・発注・取消の要求と結果、
HTTP通信の結果、paper tradingの約定や残高の変化などが含まれます。
保存したデータをプログラムから読み出すことで、操作の履歴を追跡したり、成功・失敗の件数を集計したりできます。

## 記録を始める

記録するには、まず保存先を指定して `ExecutionRecorder` を作ります。
次に、取引操作に使う `BitbankTradingVenue` または `PaperTradingVenue` の
`with_recorder` メソッドに、そのレコーダーを渡します。
以後、そのオブジェクトを通じて行った操作が記録されます。
`with_recorder` を呼ばなければ、記録用のファイルは作られず、通常どおり取引操作だけを行います。

次の例は、live tradingの口座情報を取得し、その要求と結果を `execution.jsonl` に保存します。
`JsonlSink` は、1行に1件のJSONを書き出す保存先です。

```rust,no_run
use bitbankutil_rs::{
    bitbank_private::BitbankPrivateApiClient,
    execution_recording::{ExecutionRecorder, JsonlSink},
    trading_venue::{BitbankTradingVenue, TradingVenue},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("BITBANK_API_KEY")?;
    let api_secret = std::env::var("BITBANK_API_SECRET")?;

    // 1回の実行を識別するIDと、実行について残したい情報を指定する。
    let recorder = ExecutionRecorder::new(
        "unique-run-id",
        serde_json::json!({
            "strategy": "my_strategy",
            "code_version": "revision",
            "settings": {}
        }),
        JsonlSink::open("execution.jsonl")?,
    );

    let client = BitbankPrivateApiClient::new(api_key, api_secret, None);
    let venue = BitbankTradingVenue::new(client).with_recorder(recorder.clone());

    let result = venue.account_snapshot("btc_jpy").await;

    // すべての操作が終わったら、終了を記録してバッファの内容を書き出す。
    recorder.finish()?;
    println!("{result:?}");
    Ok(())
}
```

`recorder.clone()` は、新しい記録やファイルを作る操作ではありません。
同じ記録先を利用するためのハンドルを複製します。
この例では、取引操作を記録する `venue` と、最後に `finish()` を呼ぶコードが、
1つのレコーダーを共有しています。

paper tradingでは、作成した `PaperTradingVenue` に `with_recorder(recorder.clone())` を呼びます。
この時点の残高・未約定注文・約定モデルの設定も保存されるため、取引操作を始める前に設定してください。
live tradingでは、`BitbankTradingVenue::with_recorder` が内部のAPIクライアントにもレコーダーを設定するので、
HTTP通信を記録するために別途設定する必要はありません。

`TradingVenue` を使わずにAPIクライアントを直接呼ぶ場合は、
`BitbankPrivateApiClient` または `BitbankPublicApiClient` の `with_recorder` にレコーダーを渡すと、
HTTP通信を記録できます。

## 保存した記録を読む

`read_records(BufReader::new(File::open(path)?))` で、JSONLファイルを
`ExecutionRecord` の一覧として読み出せます。
paper tradingを実行し、記録の保存から読み出し・集計まで行うサンプルも用意しています。
実際のAPIには接続しません。

```sh
cargo run --example record_execution -- /tmp/execution.jsonl
```

このサンプルでは、部分約定・手数料・取消・操作の失敗・最終残高を確認し、
操作の種類ごとに成功・失敗の件数を表示します。

各レコードは、形式バージョン1の `ExecutionRecord` として保存されます。
`run_id` は実行の識別子、`sequence` はその実行内での記録順序、
`kind` は記録の種類、`data` は記録内容です。
実行IDは、利用者が実行ごとに異なる値を指定してください。
ファイルには追記されるため、複数の実行を保存した場合は `run_id` で分けて集計します。
金額・数量は、精度を保つためDecimalの文字列形式で保存します。

要求と結果や、操作とその契機を対応付けるために、
`operation_id` と `market_event_id` も記録します。対応する情報がない場合は空欄です。
主な記録の種類は次のとおりです。

- `operation_started` / `operation_finished`: 口座取得・発注・取消の要求と結果です。
  `operation_id` で対応付けます。終了側の `operation` と `success` から操作別の件数を集計できます。
  開始だけが残った操作は、結果不明として扱います。
- `http_started` / `http_finished`: HTTPリクエストの各試行です。
  `request_id` と `attempt` で対応付けます。たとえば、live tradingの口座取得1操作には、
  注文一覧と残高を取得する2回のHTTP通信が伴います。
  終了側の `sent` がfalseなら、送信前にリクエストの構築に失敗しています。
  HTTPステータスは応答ハンドラーまで届いた場合のみ保存できます。
  エラーについては失敗した段階と取引所のエラーコードを保存し、エラーの全文は保存しません。
- `paper_initial_state` / `paper_event`: paper tradingの記録開始時の設定・口座状態と、
  注文の受理・拒否・取消・約定、その直後の残高・未約定注文です。
  この保存処理は、模擬エンジンの `drain_events` で取得できるイベント履歴を消費しません。
- `market_event`: botが受け取った市場イベントの参照情報です。
  そのイベントを処理する戦略が直接行った操作に関連付けます。生の板データは保存しません。
  模擬約定の記録には、元になった取引のIDと約定時刻も含まれます。

## 判断理由や時刻を追加する

戦略が判断した理由なども残したい場合は、
`recorder.record("strategy_decision", serde_json::json!({...}))` で独自の情報を追加できます。

時刻は用途別に分けています。`recorded_at_ms` は記録を書き出す処理を行った実時刻、
`event_time_ms` は利用者が指定する実行環境上のイベント時刻です。
取引所の約定時刻は、約定情報の中に別途保存します。

`scope(RecordingContext { ... }, future).await` を使うと、その非同期処理中に作られる記録に
イベント時刻や操作・市場イベントとの関連を設定できます。
別のタスクを生成して処理を引き継ぐ場合は、`current_context()` で取得した値を、
生成先のタスクでも `scope` に渡してください。

## 書き出しと保存失敗の扱い

標準の `JsonlSink` は、ファイルへの追記とバッファ付き同期書き込みを使います。
記録順の採番と書き込みは、レコーダーが共有するロックで直列化します。
別の保存先を使いたい場合は、`EventSink` を実装して `ExecutionRecorder::new` に渡してください。

実行を終えるときは、botを停止して処理中の操作をすべて待ち、
最後に `finish()` を1回呼んでください。終了イベントを記録し、バッファの内容を書き出します。
実行中にバッファの内容を書き出したい場合は、`flush()` を呼べます。
どちらも、電源断に対する永続性を保証するものではありません。

保存先のファイルを開けない場合は、`JsonlSink::open` がエラーを返します。
実行中の書き込みに失敗した場合は、取引を継続し、以後の記録書き込みを停止します。
異常は `status()` の `error` で確認でき、`flush()` と `finish()` もエラーを返します。

`status()` の `dropped_records` は、確実に記録できなかった呼び出しの数です。
書き込み失敗時には、バッファ内のデータや途中まで書けた行が保存されているかどうかも
不確かになるため、`persistence_uncertain` がtrueになります。
ファイルの末尾に壊れた行が残っていると、`read_records` は読み出しエラーを返します。

## live tradingで記録できる範囲

live tradingでは、通常の取引操作に伴うAPI応答から取得できた情報を保存します。
記録するためだけに追加の通信を行うことはありません。
口座情報は、それぞれのHTTP応答を取得した時点で観測した値です。
注文一覧と残高を同時に確定した状態や、完全な約定履歴を表すものではありません。

発注に成功しても、その注文が約定したとは限りません。
記録するのは、API応答から取得できた注文状態や約定数量です。
Public APIについては通信の結果・ステータスを保存し、レスポンス本体は保存しません。
購読トークン、認証ヘッダー、署名も保存しません。
利用者が渡す実行メタデータや診断情報にも、秘密情報を含めないでください。
