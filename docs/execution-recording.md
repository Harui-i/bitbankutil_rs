# 実行記録

`execution_recording` は、botが行った取引操作とその結果を保存し、実行後に調べるためのモジュールです。
たとえば「発注が失敗した理由を調べたい」「APIを何回呼んだか数えたい」
「paper tradingで注文や残高がどう変わったか確認したい」ときに使います。

live tradingとpaper tradingのどちらでも利用できます。記録には、口座情報の取得・発注・取消の要求と結果、
HTTP通信の結果、paper tradingの約定や残高の変化などが含まれます。
保存したデータをプログラムから読み出すことで、操作の履歴を追跡したり、成功・失敗の件数を集計したりできます。

## 記録を始める

記録するには、まず保存先を指定して `ExecutionRecorder` を作ります。
live tradingでは、取引操作に使う `BitbankTradingVenue` の `with_recorder` に、
そのレコーダーを渡します。以後、そのオブジェクトを通じて行った操作が記録されます。
`with_recorder` を呼ばなければ、記録用のファイルは作られず、通常どおり取引操作だけを行います。
paper tradingでは、エンジンを作るときにレコーダーを渡します。設定方法は後述します。

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

paper tradingでは、`PaperExecutionEngine::new(config, PaperClock::Realtime, balances, Some(recorder.clone()))` のように、
第4引数にレコーダーを渡してエンジンを作ります。記録しない場合は `None` を渡してください。
backtestでは `PaperClock::Replay { now_ms: 0 }` を選択します。時計の契約は [paper tradingの約定モデル](paper-execution.md) を参照してください。
そのエンジンを `PaperTradingVenue::new(engine)` に渡して、取引操作に利用します。
エンジン作成時に初期残高・約定モデルの設定を保存し、その後の注文や残高の変化も記録します。

paper tradingの記録先は、エンジンの初期化時に決まり、後から変更するAPIはありません。
`PaperTradingVenue` をクローンした場合も、`from_shared()` で同じエンジンを使う場合も、
操作の要求・結果と注文・残高の変化は、すべてエンジンに設定したレコーダーへ記録されます。
別の実行として記録したい場合は、新しいレコーダーとエンジンを作成してください。

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

各レコードは、形式バージョン2の `ExecutionRecord` として保存されます。
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
  終了側の `send_attempted` がfalseなら、リクエストの構築に失敗しています。
  trueでも、実際にパケットが送信されたことや取引所に到達したことは保証しません。
  HTTPステータスは応答ハンドラーまで届いた場合のみ保存できます。
  エラーについては失敗した段階と取引所のエラーコードを保存し、エラーの全文は保存しません。
- `http_request_started` / `http_request_finished`: 再試行を含む1つのHTTP要求です。
  `request_id` で対応付け、終了側の `attempts` は実行した試行数です。
- `paper_settlement` / `paper_delay_assumption`: 公開約定の確定境界・未確定状態・評価異常と、任意の遅延上限の仮定です。
- `paper_initial_state` / `paper_event`: paper tradingの記録開始時の設定・口座状態と、
  注文の受理・拒否・取消・約定、その直後の残高・未約定注文です。
  エンジンの `drain_events` で取得できるメモリ上の履歴は最新1024件に制限され、
  超過すると古いイベントから破棄されます。破棄された累計件数は `discarded_event_count()` で確認できます。
  この上限は永続記録には適用されず、保存失敗の有無は `status()` で確認できます。
- `market_event`: botが受け取った市場イベントの参照情報です。
  そのイベントを処理する戦略が直接行った操作に関連付けます。生の板データは保存しません。
  paper tradingでの約定の記録には、元になった取引のIDと約定時刻も含まれます。

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
注文管理関数が内部で生成する発注・取消タスクには、ライブラリが自動的にコンテキストを引き継ぎます。

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

## API時刻計測と対応付け（形式バージョン2）

`operation_id` は論理操作、`data.request_id` はHTTP要求、`data.attempt` は1始まりの試行番号です。
IDは同じレコーダー内で採番されるため、別の実行との照合では必ず `run_id` も使います。
`BitbankTradingVenue` の並行した操作には別の操作IDが付き、その非同期コンテキストをHTTP記録にも引き継ぎます。
APIクライアントを直接呼んだ場合、既存の操作コンテキストがなければHTTP要求単位で操作IDを採番します。
直接呼び出しでは `operation_started/finished` は生成せず、`http_request_*` を要求全体の単位として読みます。

注文関連の `http_started` と `http_request_started` の `data.request` には、
許可したフィールドだけを保存します（pair、order_id / order_ids、発注引数）。
発注前には注文IDがないので、要求IDから `http_finished.data.result.order_id` をたどります。
単一取消・照会は要求の `order_id` と結果を対応付け、一括取消は要求の `order_ids` と
結果の `orders` を注文IDで突き合わせます。配列の位置で対応付けてはいけません。

応答に含まれる `ordered_at` / `canceled_at` は注文ごとの取引所Unix時刻（ミリ秒）として保存します。
フィールドが存在しなければ省略し、明示的なnullはnullのまま保存します。0やローカル時刻で補完しません。
一括取消で一部の注文が返らなかった場合、その注文の取消結果・時刻は未観測です。
要求全体の成功から全注文の取消成功を推定せず、返却された各注文の `status` も確認します。
全体が失敗した応答では `result` はnull、`exchange_code` は取得できた場合だけ値を持ちます。

各終了イベントの `data.timing` は次の3項目です。開始イベントにも同じ `started_at_unix_us` を保存します。

| 項目 | 単位と意味 |
| --- | --- |
| `started_at_unix_us` | ローカルUTCのUnixマイクロ秒、計測開始時 |
| `completed_at_unix_us` | ローカルUTCのUnixマイクロ秒、計測完了時 |
| `elapsed_us` | Rustの `Instant` による単調時計の経過マイクロ秒 |

整数化ではマイクロ秒未満を切り捨てます。格納単位は時計の実分解能・正確さを保証しません。
開始UTCと単調時計、および完了時の2時計は厳密な同時読み取りではありません。
UTCはOSの時計補正で逆行し得ますが、`elapsed_us` はUTC差分を使わないため影響を受けません。
`recorded_at_ms` はレコーダーのロック取得後、sinkへ渡す直前の実時刻（ミリ秒）です。
計測対象の時刻や、ディスクに永続化できた時刻ではありません。

計測境界は次のとおりです。

- HTTP試行: `http_started` を書く直前から、基盤HTTPクライアントが結果を返した直後まで。
  開始記録のJSON生成・ロック待ち・同期sink書き込み、HTTP要求の組立・認証、接続待ち、通信、
  応答本文の読み取り、観測用JSON解析と基盤の応答ハンドラーを含みます。
  終了記録の生成・ロック待ち・書き込み、再試行の待機、型付き注文レスポンスへの変換は含みません。
  これはローカルの観測点であり、パケット送信時刻ではありません。
- HTTP要求全体: ハンドラー設定・ID採番・記録用要求の抽出の後、`http_request_started` を書く直前から、
  最後の試行の `http_finished` を書いた直後まで。全試行・再試行待機・途中の記録処理を含み、
  `http_request_finished` の書き込みと、APIクライアント側の型変換は含みません。
- 論理操作: 操作ID採番の後、`operation_started` を書く直前から、操作のfutureが完了した直後まで。
  途中のHTTP・待機・型変換・記録処理を含み、`operation_finished` の生成・書き込みは含みません。
  paper trading / backtestでもこの値は処理を行った実時間であり、仮想時刻の差ではありません。

開始記録による遅延も計測範囲に入るため、重いsinkや並行記録のロック競合は計測値を増やします。
記録の有無による実行時間への影響は残ります。要求全体・論理操作の時間を試行時間と混ぜて集計しないでください。
既存の `request took` ログは別の境界なので、構造化された試行時間と同一視できません。

`success` はその階層の成功です。HTTP試行では基盤の応答ハンドラー成功までを意味し、
その後の型変換に失敗すると `operation_finished` は失敗し得ます。
`stage` は success / build / send / receive / response、`timeout` は基盤HTTPエラーのタイムアウト判定、
`will_retry` は次の試行を行う予定です。タイムアウトでも取引所側で操作された可能性が残ります。
送信段階の失敗だけを設定された回数・間隔で再試行する既存の方針は変更しません。
成功応答・取引所エラー・応答処理失敗には追加の再試行を行いません。
途中でタスクを中断した場合、開始だけが残り、`will_retry` がtrueでも次の試行がないことがあります。

形式2ではHTTP要求全体のイベントと時刻を追加し、誤解しやすい `sent` を `send_attempted` に変更しました。
古い形式1のデータにはこの計測値はありません。

## 収集環境・時計同期のメタデータ

後から市場受信記録と照合できるよう、`ExecutionRecorder::new` のメタデータに
実行・ホスト・収集セッションの識別情報を指定します。たとえば次のJSONを `run_started` に残せます。
これは利用者が用意する任意の項目であり、自動収集や時計設定の変更は行いません。

```json
{
  "code_version": "git-revision",
  "strategy": "my_strategy",
  "environment": {
    "host_id": "tokyo-bot-01",
    "region": "tokyo",
    "os": "Linux",
    "runtime": "rust-version / tokio-version",
    "market_capture_session_ids": ["capture-session-unique-id"]
  },
  "clock_sync": {
    "source": "chrony",
    "observed_at_unix_us": 1793606400000000,
    "synchronized": true,
    "offset_us": null,
    "uncertainty_us": null,
    "note": "offsetの符号・定義、確認コマンド・時点、誤差の評価方法を記載"
  }
}
```

不明な値は省略またはnullにします。同期状態は観測時点の情報で、実行全体の保証ではありません。
途中の観測は `recorder.record("clock_sync_observed", json!(...))` で追加できます。
別ホストの市場受信時刻と比較するときは双方の時計情報が必要です。
取引所タイムスタンプにも時計差・付与位置の不確かさが残ります。
API全体の所要時間から注文有効化の片道遅延は確定せず、単純に半分にした値も実測値とは扱いません。
この記録は将来の校正材料であり、注文の有効時刻・約定判定を変更するものではありません。

## 所要時間の集計例

保存済みファイルを読むだけのサンプルを用意しています。APIへの接続はありません。

```sh
cargo run --example summarize_execution -- execution.jsonl
```

実行ID・操作・エンドポイント・結果別に、論理操作、HTTP要求全体、HTTP試行を分けて集計します。
件数、時刻のある件数、標本内の最小・中央値（偶数個では下側）・最大と、ミリ秒単位のヒストグラムを表示します。
タイムアウトとその他の失敗は別行で表示し、開始だけ残った記録は結果不明として件数を示します。
母集団のp95等は表示しません。標本数、収集期間・時間帯、障害や負荷、同期情報、記録失敗も確認して評価してください。
`http_request_finished` の `success`・`timeout`・`stage` は最終試行の結果です。
途中でタイムアウトしても再試行で成功した要求は、requestの行ではsuccessとして集計します。
途中で失敗した試行も含めたい場合はattemptの行を使います。
`run_id` はファイル内で再利用しないでください。
