# 運用ログ

ライブラリは `tracing` のイベントとspanを発行します。subscriberの初期化、出力形式、保存先、フィルターは呼び出し側の責任です。subscriberを設定しなくても取引APIは利用できます。公開関数の引数・戻り値と、注文順序・再試行・停止条件は変更していません。

`ExecutionRecorder` は別の仕組みです。取引記録・再生のJSONL形式と既存の設定を維持します。運用ログはフィルター可能なので、永続的な取引監査記録の代わりにはなりません。

## 呼び出し側の設定

アプリケーションの `Cargo.toml` に追加します。このクレート自身では `tracing-subscriber` はexamples・テスト用の開発依存です。

```toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
```

プロセスの起動時に一度だけ初期化します。examplesもこの設定で、既定ではINFO以上を標準エラーへ出します。

```rust
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into()))
    .with_writer(std::io::stderr)
    .init();
```

`init()` は、既定の `tracing-log` 機能により依存ライブラリの `log` イベントも取り込みます。`env_logger` など別のグローバルloggerを同時に初期化しないでください。

```sh
RUST_LOG=info,bitbankutil_rs=debug cargo run --example best_mm -- --paper mona_jpy 0.001 8000 0.001 0.002
```

JSONを出す場合は `.json()` を加えます。時刻はsubscriberの既定でUTCです。

```rust
tracing_subscriber::fmt()
    .json()
    .with_env_filter("info,bitbankutil_rs=debug")
    .with_writer(std::io::stderr)
    .init();
```

ファイルへ保存する場合はwriterを差し替えます。次は同期書き込みの最小例です。

```rust
let file = std::fs::OpenOptions::new()
    .create(true).append(true).open("bot.jsonl")?;
tracing_subscriber::fmt()
    .json()
    .with_env_filter("info,bitbankutil_rs=debug")
    .with_writer(std::sync::Mutex::new(file))
    .init();
# Ok::<(), std::io::Error>(())
```

保存先の権限・容量・保存期間・ローテーション・書き込み失敗の監視はアプリケーションまたは実行環境で設定してください。非同期writerを使う場合は、キュー満杯時の欠落方針を選び、欠落数の監視と終了時のflushを設定します。

## spanと識別子

- `bot_runtime` / `bot`: `runtime_id` / `run_id`。プロセス内で採番する実行識別子です。再起動をまたぐ検索には、アプリ側で実行名・ホスト名などを持つ親spanを設定してください。
- `strategy_decision`: `decision_id`、任意の `market_event_id`、`received_at_ms`、観測可能な `event_time_ms`。注文操作はこのspanの子になります。
- 注文計画: `plan_id`。個別発注は `planned_order_id`、キャンセルバッチは `cancel_batch_id` で識別します。
- `operation`: `operation_id`、`operation`。`BitbankTradingVenue` とpaper venueの操作を記録します。
- Private注文API: `api_call_id`、`pair`。低水準APIを直接呼ぶ場合にも開始・結果を追えます。
- `venue_http_request` / `venue_request` / `venue_http_attempt`: `operation_id`、`endpoint`、`method`、`venue_request_id`、`venue_attempt`。crypto-botters側の `http_request` spanの `request_id` / `attempt` とは別の識別子です。

recorderがある場合、`operation_id` はJSONLと同じ値で、運用ログの `venue_request_id` はJSONLの `request_id` に対応します。recorderがない場合もプロセス内で採番します。IDの採番元はrecorderごとに異なるため、異なるrecorderの記録は実行コンテキストと合わせて検索してください。並行FutureとBot/WSタスクには親spanを引き継ぎます。呼び出し側でspawnするタスクには `.instrument(span)` / `.in_current_span()` を使ってください。

spanはINFOで有効になります。JSON出力では `spans` 配列で親子関係を確認できます。開始だけ残る操作は、タスクの中断やpanicなどで完了まで観測できなかった可能性があります。

## 主なイベント

| event | レベルと意味 | 主なフィールド |
| --- | --- | --- |
| `order_planned`, `order_plan_created` | DEBUG: 維持・取消・発注の計画 | action, reason, order_id, 件数 |
| `planned_order_started`, `planned_order_accepted` | INFO: 注文計画からの個別発注 | span内のpair, side, price, amount; order_id |
| `planned_cancel_started`, `planned_cancel_accepted` | INFO: キャンセルバッチ | order_ids, confirmation |
| `planned_operation_finished` | WARN/ERROR: 計画内の操作失敗/結果不明 | operation, outcome, error_category |
| `order_plan_finished` | INFO/WARN: 全体終了/取消失敗による中断 | success_count, failure_count, elapsed_ms, reason |
| `operation_started`, `operation_finished` | 注文はINFO、照会成功はDEBUG、失敗はWARN、注文結果不明はERROR | operation, pair, outcome, order_id, elapsed_ms |
| `order_submit_started`, `cancel_submit_started` | INFO: venue操作の注文条件 | pair, side, price, amount, order_ids |
| `order_api_started`, `order_api_finished` | INFO: 低水準注文APIの開始・受理、WARN: 拒否、ERROR: 結果不明 | operation, 注文条件, order_id, status, outcome |
| `cancel_order_observed` | INFO: バッチ応答に実際に含まれた注文 | order_id, status |
| `venue_http_request_started`, `venue_http_request_finished` | DEBUG: HTTPリクエスト全体 | venue_request_id, attempts, stage, success, timeout, elapsed_us |
| `venue_http_attempt_started`, `venue_http_attempt_finished` | DEBUG: 通常通信、WARN: 通信失敗/取引所エラー | venue_request_id, attempt, stage, http_status, exchange_code, timeout, will_retry, elapsed_us |
| `response_unknown_fields`, `response_failed`, `response_decode_failed` | WARN/ERROR: 応答の互換性・変換・通信エラー | api, count, stage, timeout, line, column |
| `bot_started`, `bot_stopped`, `bot_failed` | INFO: 起動終了、ERROR: 実行失敗 | requested, stage |
| `strategy_started`, `strategy_finished`, `strategy_failed` | DEBUG: 判断処理、ERROR: 戦略失敗 | decision span, success, elapsed_ms |
| `market_feed_start_failed` | ERROR: 市場フィードの開始失敗 | pair |

| `ws_decode_failed`, `ws_delivery_failed`, `ws_delivery_closed` | WARN: 不正入力・満杯、DEBUG: 受信先終了 | reason |
| `market_conversion_failed`, `feed_terminated`, `feed_stopped` | WARN/ERROR: 入力異常、DEBUG: 受信先終了 | pair, reason |
| `paper_order_*`, `paper_cancel_pending` | INFO: paperの受理・取消・約定、WARN: 拒否 | order_id, price, amount, fee_amount_quoteなど |
| `recording_failed` | ERROR: recorderで最初に観測した永続化障害 | error_kind, persistence_uncertain |

`task_ended` はBot/WSタスクの終了をDEBUGで記録します（中断やpanicを含み、成功とは限りません）。`bot_join_failed` はjoin時の異常をERRORで記録します。`bot_input_received` はライブactorがキューから取り出した入力の受信時刻と処理時刻をDEBUGで記録します。既存のlive runnerのdecision spanにある `received_at_ms` はrunnerへの取り込み時刻であり、コールバック時刻とは区別してください。

`elapsed_ms` / `elapsed_us` は単調時計による経過時間です。市場のイベント時刻とローカル受信時刻は別フィールドです。取引所応答のstatusは、安全に整形した実際の値です。

`accepted` はAPIが成功応答を返したという意味です。発注の約定やキャンセル後の資金解放までを保証しません。バッチの成功から、応答に含まれない注文の取消を推定しません。`unknown` は適用有無を確定できない状態です。通信障害、応答変換失敗、一部の取引所内部エラーなどを保守的に分類します。ログ追加による自動照会・新規発注停止・再試行変更は行いません。

`best_mm` は `strategy_waiting` / `strategy_ready` を板の準備状態が変化したときにINFOで記録し、継続待ちはDEBUGにします。`strategy_evaluated` には最良気配・目標価格・必要な残高・数量制限・板時刻をDEBUGで記録します。`strategy_target_order` は目標注文をINFOで記録し、`strategy_skipped` は見送り理由を記録します。

## 秘密情報と障害時の確認

APIキー・secret・署名・認証トークン、ヘッダー/本文全体、任意のエラーDebug出力は記録しません。外部文字列は原則128文字までに制限し、制御文字をエスケープします。価格・数量は浮動小数点へ変換しません。ログ自体には注文条件や戦略情報が含まれるため、閲覧権限を制限してください。依存ライブラリや独自subscriberの出力内容は別途確認してください。

recorderの最初のappend/flush障害を `recording_failed` で通知します。以後は同じ障害を繰り返し出力せず、従来どおり `status()` の欠落件数・永続化不確実性と、`flush()` / `finish()` の戻り値で確認できます。運用ログwriterの障害とは別です。

通知・監視の起点は、注文の `outcome=unknown`、`strategy_failed` / `bot_failed`、`recording_failed`、継続するHTTP失敗や入力欠損です。通知先・閾値は呼び出し側で設定します。

## crypto-botters-bitbank 0.8.1との役割分担

通信・接続・再接続の詳細は下位クレートのtracingに任せます。bbutilは注文判断・操作結果・配送失敗・市場入力の完全性を記録します。WSコールバックでは下位側の接続spanを維持し、`connection_id` や再接続 `generation` と関連づけます。paperの再接続状態の監視と不完全性のマーキングは維持します。

bbutilは実行記録のため、下位クライアントの `max_try=1` と外側の再試行ループを維持します。1つの `venue_request_id` に複数の `venue_attempt` が対応し、各試行で下位側の別の `request_id` が発行されます。下位側の `attempt=1` は正常です。bbutilのHTTPイベントは `venue_http_*` として、取引所コード・再試行予定・記録との対応を提供します。JSONLのフィールド名と形式は変更しません。
