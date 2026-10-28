# 実行記録

同じ `ExecutionRecorder` を取引先に設定すると、口座取得・発注・取消の要求と結果、
実HTTP通信、模擬注文・約定と口座状態をJSONLに記録できます。
既存のコンストラクタは記録を無効にしたまま利用できます。

```rust,no_run
use bitbankutil_rs::{execution_recording::{ExecutionRecorder, JsonlSink},
    bitbank_private::BitbankPrivateApiClient, trading_venue::BitbankTradingVenue};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("BITBANK_API_KEY")?;
    let api_secret = std::env::var("BITBANK_API_SECRET")?;
    let recorder = ExecutionRecorder::new("unique-run-id", serde_json::json!({
        "strategy": "my_strategy", "code_version": "revision", "settings": {}
    }), JsonlSink::open("execution.jsonl")?);
    let client = BitbankPrivateApiClient::new(api_key, api_secret, None);
    let _venue = BitbankTradingVenue::new(client).with_recorder(recorder.clone());
    // ボットを終了し、処理中の操作をすべて待ってから呼ぶ。
    recorder.finish()?;
    Ok(())
}
```

模擬取引は `PaperTradingVenue::with_recorder` で有効化します。初期状態を残すため、
操作開始前に設定してください。APIクライアント単独にも `with_recorder` を指定できます。
`EventSink` を実装すれば保存先を差し替えられます。標準の `JsonlSink` は追記と
バッファ付き同期書き込みを使用します。記録順の採番と書き込みは共有ロックで直列化します。

通信なしで部分約定・手数料・取消・失敗・最終残高と操作件数を確認する例:

```sh
cargo run --example record_execution -- /tmp/execution.jsonl
```

読み出しには `read_records(BufReader::new(File::open(path)?))` を使用します。
各行は `ExecutionRecord`（形式バージョン1）で、`run_id`、`sequence`、
`recorded_at_ms`、任意の `event_time_ms`、`operation_id`、`market_event_id`、
`kind`、`data` を持ちます。実行IDは利用者が一意に指定し、複数実行の追記ファイルでは
実行IDでグループ化してください。金額・数量はDecimalの文字列形式です。

- `operation_started` / `operation_finished`: 操作IDで要求と結果を対応付けます。
  終了側の `operation` と `success` を集計すれば操作件数になります。
  開始だけが残った操作は結果不明として扱います。
- `http_started` / `http_finished`: `request_id` と `attempt` で各試行を対応付けます。
  終了側の `sent` がfalseのものは送信前の構築失敗です。口座取得は1操作・2通信です。
  HTTPステータスは応答ハンドラーまで届いた場合のみ記録できます。
  エラーは段階と取引所コードを残し、外部エラーの全文は保存しません。
- `paper_initial_state` / `paper_event`: 設定・約定モデル、注文イベントと直後の
  残高・未約定注文を保存します。既存の `drain_events` は従来どおり利用できます。
- `market_event`: ボットが受け取った市場イベントの参照です。生の板データは保存しません。
  戦略内の直接の操作と関連付けます。模擬約定には元取引のIDと約定時刻も含まれます。

`recorder.record("strategy_decision", serde_json::json!({...}))` で診断情報を追加できます。
`scope(RecordingContext { ... }, future).await` でイベント時刻や関連を明示できます。
`current_context()` の値を引き継げば、別途spawnするタスクでも関連を維持できます。
記録時刻と取引所の約定時刻、利用者指定のイベント時刻は別物です。

保存先のオープン失敗は `JsonlSink::open` が返します。実行中の書き込み失敗は取引を継続し、
以後の記録書き込みを停止します。`status()` の `error`、`dropped_records`、
`persistence_uncertain` で検知でき、`flush()` / `finish()` もエラーを返します。
バッファ内のデータや途中まで書けた行の保存状態は不確かなため、欠落件数は確実に
記録できなかった呼び出しの数です。壊れた末尾行は読み出しエラーになります。
`finish` は呼び出し元が一度だけ、操作終了後に呼んでください。flushは電源断への永続性を保証しません。

実取引の口座情報は既存の各HTTP応答の取得時刻における観測です。
複数APIをまたぐ原子的な初期状態や完全な約定履歴ではありません。
発注成功を約定とは扱わず、取得できた注文状態・約定数量だけを保存します。
記録のための追加通信は行いません。Public APIは通信の結果・ステータスを記録し、
レスポンス本体は保存しません。購読トークン、認証ヘッダー、署名は保存しません。
**利用者が渡すメタデータ・診断情報にも秘密情報を含めないでください。**
