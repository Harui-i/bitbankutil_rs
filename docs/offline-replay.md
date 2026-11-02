# オフライン戦略実行とbacktest

過去の市場データを使ったbacktestでは、逐次ランナーと市場データの再生機能を組み合わせます。

`BotRunner::new(strategy, venue)` は通信を起動しないランナーです。
市場の登録と戦略への配送を分け、ハンドラーは1件ずつ実行します。
遅延0の `process(time_ms, input_id, event).await` は、口座更新、戦略、
`ctx.emit()` による派生イベントまで処理します。直接入力の `process` は待機中の模擬操作の
内部期限も駆動するため、その途中に別の市場入力があり得る収録データには `run_replay` を使ってください。
`submit_replay` は指定した受信時刻で入力を登録し、将来へ時計を進めずに模擬操作待ちを返す低水準APIです。
`BotStrategy` は関連型 `Error` と `Result<(), Self::Error>` を返すハンドラーを実装します。

戦略の時刻判定には `ctx.now_ms()` を使います。通常のWebSocket実行は実時計、
再生では現在のローカル仮想時計です。模擬操作を待つ間に時計が進むので、await前後で値が変わり得ます。
仮想時刻は非減少で、同時刻の複数イベントを許可します。入力の受信時刻は書き換えません。
待機中に受信した市場入力はその時刻でvenueへ1回登録し、戦略への配送だけをキューへ保留します。
その入力のハンドラーが後で起動したときの `ctx.now_ms()` は配送時の実行時計です。
`input_registered.received_at_ms` と配送時の記録 `event_time_ms` は区別してください。

`emit` による派生イベントはFIFOで、待機中に保留した外部入力の戦略配送より先に処理します。
派生イベントも実行時点の時計と起点の入力IDを使います。既定の派生件数上限は10,000件で
`derived_limit` から変更できます。配送保留キューは `delivery_limit`（既定10,000件）で制限し、
超過は入力を黙って捨てず `DeliveryLimit` で失敗させます。
戦略がemitのエラーを無視した場合も、派生上限超過は実行失敗になります。
処理完了後に保持したcontextからemitしてもエラーになります。

任意のsleep、interval、実時計、独立タスクは仮想化しません。
venueに未完了の模擬操作・通知がない通常の非同期ハンドラーは、従来どおり時計を固定して完了を待ちます。
模擬操作が保留されている間のPendingは、ランナーが市場入力と内部期限の処理へ戻る条件です。
任意のFutureが何を待っているかまで解析する仕組みではありません。
注文操作はハンドラーからawaitし、並行する場合も登録順序を固定してください。
組込みの並行注文管理は同一タスク内で各操作をpollします。利用者が独立したタスクをspawnした場合の
要求登録順やタスク起床順に依存する戦略の決定性は保証しません。

paper trading用のエンジンをbacktestに使う場合、コンストラクターで
`PaperClock::Replay { now_ms: 0 }` を明示します。初期時刻は最初の入力時刻以下にします。
`BotRunner::advance_time` / `process` はvenueの再生時計も更新します。
`run_replay` は、戦略に渡さない接続通知を含む各入力の受信時刻で時計を進めます。
`PaperClock::Realtime` のvenueを再生へ渡すと `InvalidClock` エラーになります。
注文有効開始時刻と取引所時刻の比較は [paper tradingの約定モデル](paper-execution.md) を参照してください。

`PaperExecutionConfig.time_model` の `PaperTimeModel::FixedOffset { exchange_minus_local_ms: delta }` で
取引所時刻E = ローカル時刻L + δを指定します。既定のδ=0も同期が検証済みという意味ではありません。
受信記録・戦略時刻はローカル時計のまま、エンジン内の比較時にだけ変換します。
板・公開約定・明示供給するwatermarkは取引所時計なので変換しません。
公開約定の遅延上限は独立した設定で、自動境界はL + δ - delay - 1msです。
時計モデルとδは `ReplayReport.settlement.time_model` にも残ります。結果を比較するときに確認してください。

## 市場データv1

`ReplayInput::open(manifest_path)` はmanifestのsource一覧とJSONLファイルを逐次読み込みます。
1 sourceは1 sessionです。manifestのsource配列順を固定し、
`(received_at_micros, source_index, sequence)` 順で決定的にマージします。
取引所時刻による入力の並べ替えや時計補正は行いません。
ランナーには `received_at_micros / 1000` を渡し、us単位のマージ順は維持します。

各行はUTF-8のJSONオブジェクトと改行です。共通項目は
`format_version: 1`、`exchange`、`stream`、`session_id`、`connection_id`、
`sequence`、`received_at_micros` です。対応する組合せはbitbank/publicとbybit/linearです。

- `kind: connected` は `endpoint` と `subscriptions` を持ちます。
- `kind: message` はコールバックに渡されたJSON全体を `message` に持ちます。
- `kind: disconnected` は `reason` を持ちます。値はtransport_closed、scheduled_refresh、shutdown、capture_gapです。

sequenceはsession内で1から連続し、接続・ファイル境界でリセットしません。
source内の時刻逆行、sequence欠落・重複、接続ID再利用、接続外message、
空行、不正JSON、改行を欠く末尾行をエラーにします。
接続は複数同時に存在でき、connectionごとに区別します。
完全な最終行の後のEOFは選択された有限入力の終端です。切断は捏造しません。
途中のファイルから開始する場合は、必要な前方ファイルをsequence 1から供給してください。
圧縮ファイルの自動展開は行いません。独自展開・変換したレコード列も `run_replay` に渡せます。
`run_replay` は入力経路によらず、形式バージョン・sourceとsessionの対応・接続状態・
sequence連続性・受信時刻を共通処理で検証します。独自入力列は同じマージキーで整列して渡してください。
マージ順の検証はmsへの変換前に行うため、同じms内のus時刻逆行もエラーになります。

Bitbankのwhole前のdiffは保留し、whole受信時にwholeより新しいdiffを数値sequence順に適用します。
wholeと同じsequenceのdiffは適用せず、取引所sequenceの数値の飛びは欠損と判定しません。
板はsource・connection・pair単位で分離し、日次ファイルをまたいで維持します。
切断でその接続の板・保留diffを破棄します。
古いwhole・重複diffは再適用しません。同一diff sequenceの最初の値を採用します。
価格は正、数量は有限の非負数、whole数量は正である必要があります。

公開約定は再生全体で `(pair, transaction_id)` の初出だけを適用し、
同一IDで内容が異なる場合は入力エラーにします。約定ID集合は再生中保持するため、
そのメモリ使用量は約定件数に応じて増えます。入力全体をメモリへ展開することはありません。
直接渡す正規化済みイベントの重複除去は呼び出し元の責務です。
Bitbankの購読応答・未知チャネルは無視し、既知チャネルの変換失敗は返します。
Bybitの生JSONとメタデータは利用者アダプターへ渡し、カスタムイベントへ変換できます。
Bybitの板復元は含みません。

## 結果と記録

`run_replay` の通常評価は切断で停止し、`incomplete` を返します。
`recovery: true` では再接続後も市場データの検証と戦略への配送を受信順に継続し、欠損区間を `gaps` に残します。
`PaperTradingVenue` も同じです。執行が未確定であることだけを理由に再生を止めません。
ただし約定確定・資金解放・新規注文は保留したままで、再接続だけでは解除しません。
戦略が発注拒否などを未処理エラーとして返した場合は、戦略エラーとして停止します。
接続の復旧と執行状態の整合性の回復は区別してください。

backtestでは実ネットワークへ再接続せず、記録中の切断・再接続を再生します。
切断から再接続までの待ち時間は記録の受信時刻に従って再生時計を進め、実時間のsleepは入れません。
再接続記録のない接続を自動的に復旧したと仮定することもありません。
独自入力では `BotRunner::process_recovering` と `TradingVenue::on_market_event_recovering` が同じ継続経路を提供します。
gapには切断した接続の購読集合も記録します。同じsourceで空でない購読集合が完全に一致し、
対応する未終了gapが1件だけの場合に、その接続開始をgapの終了として記録します。
購読配列の順序は問いません。別銘柄の接続・購読の部分一致や変更・空の購読集合・
同じ購読集合のgapが複数ある場合は、対応を断定せず未終了のまま残します。
再接続後に板が復旧しても、全体のincompleteは解除しません。
通常の入力エラーを復旧モードで握りつぶすことはありません。
`error`、`interrupted`、`incomplete` がすべて偽／空である場合だけ完全な終端到達です。
中断フラグは入力・内部イベントの境界で確認します。返却時に保留中のハンドラーFutureを破棄しますが、送信済みのvenue操作は取り消しません。

`input_id` はsessionとsequenceを記録します。市場イベント、派生イベント、戦略内の操作・診断へ
起点の入力IDを伝播し、市場イベントに由来する派生イベントは市場イベントIDも引き継ぎます。
操作・診断の実行時刻はawait復帰後の現在時刻を使い、受信時刻へ遡りません。
`recorded_at_ms` は実時計のままです。生市場データは実行記録から復元できません。
ランナーは `replay_outcome` を既存レコーダーへ記録しますが、自動でfinishしません。
最終口座の取得・残注文の取消などをawaitし、最後に `recorder.finish()` を1回呼んでください。
保存失敗は既存の `status()` とfinish/flushの戻り値で確認できます。
通常ランタイムのshutdownは入力受付を閉じ、処理失敗も返します。
模擬操作待ちのまま入力が尽きた場合はハンドラーを打ち切り、送信済み操作・未確定資金を残して確定状態を記録します。
Dropは強制停止で、正常完了を保証しません。

## サンプル

```sh
cargo run --example offline_replay -- /tmp/replay.jsonl
```

APIキー・ネットワーク・実時間sleepは不要です。fixtureは実履歴ではなく合成データです。
切断前の入力範囲で実行し、仮想時刻50ms間隔の注文更新、部分約定、手数料、取消、最終残高を保存します。
manifestの識別情報・source順・初期状態・モデル・戦略設定も記録します。
同じ入力・初期状態・設定と操作順の決まった戦略について、取引結果を再現します。
実行IDと記録実時刻、並列操作の競合を含む記録ファイルのバイト一致は保証しません。

paper/backtestの約定は公開約定のwatermarkで確定します。入力が終わっただけでは
未着取引のないことを証明できず、`ReplayReport.settlement.unsettled` と `incomplete` がtrueになり得ます。
切断・入力エラー・中断後は資金解放も保留し、再接続で正常評価に戻しません。
完了境界の供給方法と終了手順は [約定モデルと時計](paper-execution.md) を参照してください。
サンプルは合成fixtureの既知の境界を明示的に供給します。一般のcaptureのEOFにこの保証はありません。

## 仮想時刻の予約イベント

backtestの戦略は `ctx.schedule_at(local_ms, custom_event)` または
`ctx.schedule_after(delay_ms, custom_event)` で将来のカスタムイベントを予約できます。
`run_replay` も含め、再生は市場入力とこのキューを比較する同じランナーを使います。
venue内部の発注有効化・取消有効化・API応答・状態通知も、次の市場入力と期限を比較して駆動します。
遅延設定は [paper tradingの約定モデル](paper-execution.md) を参照してください。

スケジューリングの基準は**ローカル時計の整数ms**です。受信時刻をmsへ切り捨てた
同じ時刻枠では、全市場入力を元の `(received_at_micros, source_index, sequence)` 順に処理し、
模擬操作待ちなら戦略配送を保留しつつ受信登録を進め、その後に内部期限を処理します。
同時刻のvenue内部処理を戦略のカスタム予約より先に行い、カスタム予約同士は `(execute_at_ms, id)` 順です。
同じms内でも市場入力は省略・結合しません。予約処理中に追加した同時刻予約は既存予約の後ろです。
取引所時計の時刻をこのAPIに直接渡さないでください。エンジンへローカル時計を渡し、
既存の `PaperTimeModel` が境界で一度だけL + δへ変換します。公開約定時刻とwatermarkは変換しません。

予約はハンドラーが成功した後に採番・登録されます。待機中に予約時刻を過ぎた場合は
登録時点以降に実行し、時計や情報公開を遡らせません。採番はランナー内で単調増加し、
レコーダーの有無に依存しません。過去への予約・負の遅延は `ScheduleError::Past`、
加算の範囲外は `Overflow`、live実行は `NotReplay`、完了済みcontextは `Closed` です。
戦略がエラーを返したハンドラー内の予約は登録しません。既に完了した別ハンドラーの変更はロールバックしません。
未実行予約数と、同一msで実行する内部イベント数は `BotRunner::scheduled_limit`（既定10,000）で制限します。
登録上限超過を戦略が無視した場合もランナーは `ScheduledLimit` で失敗します。
同時刻の予約を生成し続けても上限で停止し、未実行分は結果に残ります。
OS時計・Tokioタイマー・タスク起床順は予約順序を決めません。独立タスクからの競合操作の決定性は保証しません。

市場イベントはvenue適用→戦略への配送を各1回行います。予約イベントはカスタムイベントとして
戦略へ配送するので、市場へ再適用しません。模擬操作待ち中も市場登録と内部イベントは進みますが、
ハンドラーを並列に起動することはありません。次の入力は1件だけ先読みし、
市場への適用は受信時刻まで、戦略への配送は先行ハンドラーの完了まで待ちます。
入力全体の事前読み込みや、模擬遅延に相当する実時間sleepは不要です。

### 評価境界と停止

`run_replay_with_options` の `ReplayOptions { recovery, end_time_ms: Some(end) }` は
ローカル時計の評価終了時刻を指定します。**境界と同じmsの入力・予約も含みます**。
最後の市場入力より後でも、指定境界まで予約を処理して時計を進めます。
空入力でも指定境界へ進めます。現在のランナー時計より前の終了境界はエラーです。
既存の `run_replay`（終了時刻指定なし）は、EOFで最終入力のmsまでの予約だけを処理します。
EOFを理由に未来の予約を無制限に処理することはありません。

`ReplayReport` は `end_reason`（eof / end_time / disconnected / error / interrupted）、
`input_exhausted`（先読みでEOFを確認したか）、`final_time_ms`、`pending_scheduled`、
`pending_handler`（終了時に未完了ハンドラーがあったか）、`pending_deliveries`（配送保留数）を返します。
範囲外の予約はランナー内に保留され、ID・予約時刻・期限・起点入力IDを実行順で確認できます。
範囲外のカスタム予約だけでは `incomplete` にはしません。未完了ハンドラーや配送保留、
venueの未応答操作・未配信通知が残る場合は `incomplete` です。実行開始した予約が失敗した場合は消費済みとし、再実行しません。
返却時に未完了ハンドラーFutureを破棄してcontextを閉じますが、送信済みvenue操作は消しません。
戦略状態をFutureから回収できないため、そのランナーで戦略処理を再開することはできず、再使用は `Stopped` エラーになります。

切断は通常モードではその場で停止し、保留予約も停止します。`recovery: true` では
市場入力と予約の両方を継続しますが、欠損は残り、venueの執行保留は解除しません。
入力不正・戦略や内部処理の失敗・中断では以後の予約を停止し、venueを不完全状態として記録します。
先読みで入力エラーを検出した場合も、その時点で停止します。
中断フラグは入力と内部イベントの境界で確認し、保留中のハンドラーは返却時に打ち切ります。
Future自体のdropによる強制中断では戻り値・終了記録を保証しないため、結果が必要なら中断フラグを使用してください。

時計の進行・予約実行・EOFは公開約定がすべて到着済みという証明になりません。
watermarkを強制flushせず、既存の明示供給または公開約定の遅延上限設定を維持します。
`scheduled_event_reserved` / `scheduled_event_executed` は予約ID、ローカル予約・実行時刻、
起点入力IDを実行記録へ残します。記録失敗やレコーダーなしの場合もスケジューリングは同じです。

ネットワークとsleepを使わない結合例は `tests/replay_scheduler.rs` と `tests/replay_order_driver.rs` です。
1000msの入力から1100ms・1200msに予約し、1300msの次入力より前に実処理されること、
同時刻の追加予約、EOF後の境界内処理、時計差δの正負・ゼロ、記録有無、失敗と中断を検証します。

```sh
cargo test --locked --test replay_scheduler
```
