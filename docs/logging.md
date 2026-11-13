# 運用ログ

ライブラリは `tracing` のイベントとspanを発行します。subscriberの初期化、出力形式、保存先、フィルターは呼び出し側の責任です。subscriberを設定しなくても取引APIは利用できます。注文順序・再試行・停止条件は変更していません。板検証の戻り値には、位置と理由を持つ `DepthValidationError` を使用します。

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

- `application`: `process_instance`（PIDとプロセス内の初回生成時のUTC）、`execution_id`、アプリ名・バージョン・ビルド・インスタンス・戦略名・口座の別名。`RunMetadata` で指定します。秘密情報は指定しません。
- `bot_runtime` / `bot`: `runtime_id` / `run_id`。プロセス内で採番する実行識別子です。再起動をまたぐ検索には親の `process_instance` / `execution_id` を使い、`RunMetadata.instance` でホストやコンテナも識別してください。
- `market_input`: `input_sequence`、WSコールバックの `received_at_ms`、channel。接続spanの `connection_id` / `generation` とともにキューを越えて保持します。
- `bot_delivery`: 入力の接続コンテキストとactorの `run_id` を関連づけます。
- `strategy_decision`: `decision_id`、`event_type`、pair、任意の `market_event_id`、`received_at_ms`、観測可能な `event_time_ms`。注文操作はこのspanの子になります。
- 注文計画: `plan_id`。個別発注は `planned_order_id`、キャンセルバッチは `cancel_batch_id` で識別します。
- `operation`: `operation_id`、`operation`。`BitbankTradingVenue` とpaper venueの操作を記録します。
- Public/Private API: `api_call_id` と `venue_request_id`。対象がある場合はpair・order_idも持ちます。低水準APIを直接呼んだ後の応答変換失敗まで、HTTPリクエストと関連づけられます。
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
| `response_unknown_fields`, `response_failed`, `response_decode_failed` | WARN/ERROR: 応答の互換性・変換・通信エラー | api, count, fields（最大8件）, truncated, stage, timeout, reason_code, field |
| `bot_configured`, `bot_started`, `bot_stopped`, `bot_failed` | INFO: 設定・起動終了、ERROR: 実行失敗 | mode, strategy_type, subscriptions（最大32件）, buffer_capacity, requested, stage, reason_code, action, pending_* |
| `strategy_started`, `strategy_finished`, `strategy_failed` | DEBUG: 判断処理、ERROR: 戦略失敗 | decision span, success, elapsed_ms |
| `market_feed_start_failed` | ERROR: 市場フィードの開始失敗 | pair, reason_code（io/tls/http_handshake/protocol/urlなど）, action |

| `ws_decode_failed`, `ws_delivery_failed`, `ws_delivery_closed` | WARN: 不正入力・満杯、DEBUG: 受信先終了 | reason |
| `market_conversion_failed` | WARN: 変換できない入力を破棄 | pair, message_type, error_kind, reason_code, field, side, level_index, transaction_index, transaction_id, event_time_ms, sequence_id, action, failure_count, suppressed_count |
| `feed_terminated`, `feed_stopped` | ERROR: フィードの異常終了、INFO/DEBUG: 受信先終了 | pair, reason_code/reason, action |
| `market_feed_state`, `market_input_recovered`, `ws_input_recovered` | INFO/WARN: 待機・準備完了・異常・無受信・復旧 | state, reason_code, message_type, failure_count, last_valid_age_ms, pending_deliveries |
| `input_failure_summary` | INFO: 入力処理の集約結果 | failure_count, suppressed_count, unresolved |
| `api_authentication_state` | INFO: Private API認証の初回成功・失敗・回復への遷移 | outcome, reason_code, security_relevant |
| `paper_order_*`, `paper_cancel_pending` | INFO: paperの受理・取消・約定、WARN: 拒否 | order_id, price, amount, fee_amount_quoteなど |
| `recording_failed`, `recording_status` | ERROR: 最初の永続化障害、INFO: 記録状況 | recorder_id, recorder_run_id, stage, error_kind, first_failure_sequence, dropped_records, persistence_uncertain |

`task_ended` はBot/WSタスクの終了をINFOで記録します。`outcome=unobserved` と `panicking` を持ち、中断やpanicを含むため成功を意味しません。`bot_abort_requested` はhandleのDropによる中断要求です。`bot_join_failed` はjoin時の異常をERRORで記録します。`bot_input_received` はライブactorがキューから取り出した入力の受信時刻と処理時刻をDEBUGで記録します。既存のlive runnerのdecision spanにある `received_at_ms` はrunnerへの取り込み時刻であり、コールバック時刻とは区別してください。

`elapsed_ms` / `elapsed_us` は単調時計による経過時間です。市場のイベント時刻とローカル受信時刻は別フィールドです。取引所応答のstatusは、安全に整形した実際の値です。

`accepted` はAPIが成功応答を返したという意味です。発注の約定やキャンセル後の資金解放までを保証しません。バッチの成功から、応答に含まれない注文の取消を推定しません。`unknown` は適用有無を確定できない状態です。通信障害、応答変換失敗、一部の取引所内部エラーなどを保守的に分類します。ログ追加による自動照会・新規発注停止・再試行変更は行いません。

`best_mm` は `strategy_waiting` / `strategy_ready` を板の準備状態が変化したときにINFOで記録し、継続待ちはDEBUGにします。`strategy_evaluated` には最良気配・目標価格・必要な残高・数量制限・板時刻をDEBUGで記録します。`strategy_target_order` は目標注文をINFOで記録し、`strategy_skipped` は見送り理由を記録します。

## 秘密情報と障害時の確認

APIキー・secret・署名・認証トークン、ヘッダー/本文全体、任意のエラーDebug出力は記録しません。外部文字列は制御文字などをエスケープした後で128バイト以下に制限します。エスケープ途中では切りません。価格・数量は浮動小数点へ変換しません。ログ自体には注文条件や戦略情報が含まれるため、閲覧権限を制限してください。依存ライブラリや独自subscriberの出力内容は別途確認してください。

recorderの最初のappend/flush障害を `recording_failed` で通知します。以後は同じ障害を繰り返し出力せず、`status()` の欠落件数・永続化不確実性と、`flush()` / `finish()` の戻り値で確認できます。`log_status()`・`finish()`・最後のrecorderのDropでは `recording_status` を出し、欠落の進行を確認できます。`first_failure_sequence` は障害を初めて観測した位置であり、実際の永続化済み境界を保証しません。運用ログwriterの障害とは別です。

通知・監視の起点は、注文の `outcome=unknown`、`strategy_failed` / `bot_failed`、`recording_failed`、継続するHTTP失敗や入力欠損です。通知先・閾値は呼び出し側で設定します。

## crypto-botters-bitbank 0.8.1との役割分担

通信・接続・再接続の詳細は下位クレートのtracingに任せます。bbutilは注文判断・操作結果・配送失敗・市場入力の完全性を記録します。WSコールバックでは下位側の接続spanを維持し、`connection_id` や再接続 `generation` と関連づけます。paperの再接続状態の監視と不完全性のマーキングは維持します。

bbutilは実行記録のため、下位クライアントの `max_try=1` と外側の再試行ループを維持します。1つの `venue_request_id` に複数の `venue_attempt` が対応し、各試行で下位側の別の `request_id` が発行されます。下位側の `attempt=1` は正常です。bbutilのHTTPイベントは `venue_http_*` として、取引所コード・再試行予定・記録との対応を提供します。JSONLのフィールド名と形式は変更しません。


## アプリの実行情報

ビルダーに非機密の実行情報を設定します。`account_alias` は人間が付けた別名で、APIキーや実口座番号ではありません。未指定の文字列は空になります。modeはvenueが `live` / `paper` / `simulation` / `market_data` を提供し、独自venueの既定は `custom` です。

```rust
use bitbankutil_rs::telemetry::RunMetadata;
let runtime = builder
    .run_metadata(RunMetadata {
        application: "usdtjpy".into(),
        application_version: env!("CARGO_PKG_VERSION").into(),
        build: "deployment-revision".into(),
        instance: "bot6".into(),
        strategy: "market-maker".into(),
        account_alias: "primary".into(),
    })
    .feed_stale_after(std::time::Duration::from_secs(30))
    .spawn();
```

ビルダー以外のAPI操作も含めたい場合は、`RunMetadata::span()` を親としてFutureを `.instrument(span)` してください。`span.enter()` のguardをawaitをまたいで保持しないでください。examplesはsubscriber設定後に `logging_initialized` を出します。

## 理由コードとエラー情報

`reason_code` は安全な分類です。取引所エラーはエラーenumのvariantを固定文字列へ写し、`exchange_code` は応答の `data.code` の数値だけを取り出します。認証関連の失敗には `security_relevant=true` が付きます。HTTP層は `connect` / `timeout` / `body` / `decode` / `request` などの観測可能な分類を記録し、再試行時は `will_retry`・`retry_delay_ms`・`action` を記録します。DNS/TLSの内訳を推測したり、秘密を含み得るエラー全文を出したりはしません。

`order_api_finished` / `operation_finished` / `planned_operation_finished` は拒否と適用結果不明を区別したうえで理由コードを持ちます。従来の `Option<BitbankHandleError>` が `None` の場合は `unavailable_error_detail` となるので、同じ `api_call_id` / `venue_request_id` の `response_failed` / `response_decode_failed` を参照します。`api_call_finished.success` は応答の型変換まで含めた結果です。

板の `DepthValidationError` は `reason_code`・`field`・`side`・0始まりの `level_index` を保持します。例: `reason_code=non_positive_amount, field=amount, side=sell, level_index=0` は全体板の先頭ask数量が正でないという意味です。`numeric_precision` は固定小数点で正確に表せない精度を意味します。外部の不正値そのものは記録しません。

REST/WSの変換失敗は `missing_field` / `invalid_type` / `unknown_variant` などとフィールドパスを持ちます。RESTでは既にJSONのValueへ変換済みなので、元本文のline/columnを原因の代わりには使いません。未知フィールドは値を含めずパスを最大8件まで出します。

戦略固有の失敗を識別する場合は `BotStrategy::error_code` を実装します。既定は `strategy_error` です。戻す文字列は固定の非機密コードにしてください。任意の `Debug` / `Display` をそのままログへ渡さないでください。Botの既知エラーは `time_reversed` / `delivery_limit` などを記録します。ユーザー定義エラーを含むsimulationの自由文は、既知の内部理由に一致する場合だけ具体化し、それ以外は `market_incomplete` に分類します。

注文引数の検証失敗は、panic前に `input_validation_failed` と `field` / `reason_code` / `send_attempted=false` を残します。条件付き注文の開始ログには `trigger_price` も含まれます。paper注文拒否には拒否理由と注文条件・不足資産を残します。

## 入力欠損・復旧・ログ量

`market_conversion_failed` は `action=drop_message` を持ちます。板検証失敗時は直前の板を保持した事実と、最後の板時刻を記録します。`market_feed_state=degraded` から、検証に成功した全体板で `ready` へ戻ります。この状態は運用上の観測で、既存の売買・板適用方針を変更しません。`market_input_recovered` は同じ入力種別で再び変換に成功した意味で、板全体の再同期を保証しません。

WSで有効入力が既定30秒届かなければ `state=stale` を一度記録し、有効入力の再開で `receiving` を記録します。これは全channelを合わせた無受信の監視で、特定channelだけの停止や時刻の正確性を保証しません。最初のスナップショット前は `waiting_snapshot` / `market_snapshot_pending` を確認します。再接続の通信詳細とgenerationは下位クレートのログを参照します。paperの入力異常はbuffer満杯・不正入力・再接続・フィード終了を区別して評価を無効にし、再接続だけで正常な損益へ戻しません。

繰り返す入力エラーは入力種別と理由コードごとに1, 2, 4, 8, 16…回目を出します。理由が変われば最初の一件を残します。イベントの `failure_count` は総失敗件数、`suppressed_count` は省略件数です。`market_input_recovered` / `ws_input_recovered` と終了時の `input_failure_summary` で件数を確認してください。集約キーは内部の固定コードだけなので、未知のchannel名や不正値によってキー数が増えません。liveの配送が1秒以上待たされた場合は `ws_delivery_delayed` と待機時間・保留件数を記録します。

強制killや電源断ではDrop・最終集約・flushを実行できないことがあります。独立した監視でプロセスとログ収集の生存を確認してください。

## 運用ログwriterの監視

`ExecutionRecorder` とsubscriberのwriterは別です。`telemetry::LoggingHealth` はwriterの書き込み/flush失敗、最後のエラー種別、書き込んだバイト数、最後の書き込み成功時刻を追跡します。writer内から再帰的にログを出さず、失敗も呼び出し元へ返します。

```rust
use bitbankutil_rs::telemetry::LoggingHealth;
let health = LoggingHealth::default();
let file = std::fs::OpenOptions::new()
    .create(true).append(true).open("bot.jsonl")?;
tracing_subscriber::fmt()
    .json()
    .with_env_filter("info,bitbankutil_rs=debug")
    .with_writer(std::sync::Mutex::new(health.wrap(file)))
    .init();

// 独立した監視タスクから定期的に確認し、変化したときに別の経路へ通知します。
let status = health.snapshot();
if status.write_failures > 0 || status.flush_failures > 0 {
    eprintln!("logging writer unhealthy: {:?}", status);
}
# Ok::<(), std::io::Error>(())
```

ライブラリは保存先・保存期間・ディスク容量上限を決めません。アプリのデプロイ設定でファイル権限、ローテーション/保持上限、残容量、収集停止、非同期writerを使う場合の欠落件数と終了時flushを設定します。異常を通知する経路は、壊れたwriterと独立させてください。`last_successful_write_ms` の停滞は、アプリが正常時にも出力する頻度と合わせて判定します。ログへのアクセス権、転送・保管の保護もデプロイ先で管理します。

この分担は [OWASP Logging Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html) の Event attributes / Data to exclude / Verification に基づきます。回帰テストでは理由・位置・相関・集約件数・復旧、CR/LFと長い文字列、秘密情報非出力、記録sinkおよび運用writerの失敗を確認します。


## 0.5.0への移行

`BitbankDepth::insert_diff` / `update_whole` と `MarketEventConversionError::InvalidDepth` のエラーは、文字列から `DepthValidationError` へ変更しました。`reason_code` / `field` / `side` / `level_index` を参照するか、必要ならDisplayで表示してください。`BotStrategy::error_code` と `TradingVenue::mode` には既定実装があります。運用ログのフィールド・レベル・集約方針はこの文書に合わせて更新してください。

## 注文APIのレイテンシ校正

`post_order` / `post_cancel_order` / `post_cancel_orders` はINFOの
`order_api_timing` に `request_started_at_ms` と `response_completed_at_ms`
（ローカルUnix時刻、ミリ秒）、`elapsed_ms`（単調時計）、`success` を記録します。
開始はtransport呼び出し直前、完了はそのFutureが戻った直後です。署名・通信・再試行・
バックオフ・JSON読み取りを含み、ソケット送受信だけの時刻ではありません。
`elapsed_ms` は従来のAPI処理開始からの計測を維持し、引数検証や本文構築、応答型変換も含みます。

成功時の `order_api_finished` は新規注文の `ordered_at`、単一取消の `canceled_at` を
取引所のUnix時刻（ミリ秒）として記録します。まとめ取消は各 `cancel_order_observed` に
`order_id` と `canceled_at` を記録します。欠落した取消時刻はフィールドを省略し、
要求時刻や別注文の時刻で埋めません。返却されなかった注文の取消成功も推定しません。
既存の必須 `ordered_at` が欠落した新規応答は従来どおり変換失敗になります。

JSON subscriberの `spans` にある `api_call_id` と `venue_request_id` で時刻イベントと
成功イベントを結合し、まとめ取消はさらに `order_id` で区別してください。
プロセスを跨ぐ集計には `RunMetadata` の `process_instance` も使用します。
追加のPrivate API照会は行わず、API呼び出し数・注文や取消の実行順序も変更しません。

校正ではローカル開始を S、完了を R、取引所の注文／取消時刻を E とし、
独立に推定した時計差 Δ（取引所時計 − ローカル時計）で E' = E − Δ に補正します。
`entry_ms` の候補は E' − S、`response_ms` の候補は R − E' です。
この分解には取引所内部の処理も含まれます。Δ の推定誤差が ±u ms なら、
各候補にも少なくとも ±u ms の不確実性があり、ミリ秒への丸め誤差も加わります。
時計差未補正の値を片道レイテンシと断定しないでください。

新規・単一取消・まとめ取消を別々に集計し、再試行のない成功応答から分布を確認します。
`venue_attempt` が複数ある要求は別集計にし、既存注文の取消結果が再返却されたケースは
今回の受付時刻とみなさず除外します。まとめ取消の R は共通でも E は注文ごとに異なります。
時計補正後に E' が [S, R] の外に出る観測は、時計差・時計の補正・既存結果などを調べ、
ゼロへの丸めで隠さないでください。壁時計の R − S と単調時計の所要時間の不整合も確認します。
RTTだけから entry / response を一意には決定できません。
観測RTTをそのまま `entry_ms` に代入せず、時計差の根拠・誤差幅・除外条件とともに
`PaperLatencyConfig` の値を選びます。この計測は設定を自動変更しません。
