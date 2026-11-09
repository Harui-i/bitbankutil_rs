# Paper・バックテストの市場と時計

通常のPaper（`BitbankBotBuilder`）とバックテスト（`run_replay`）は、
`SimulationDriver` による同じ仮想時間の実行基盤を使います。
内部取引所は市場の発生時刻で状態を更新し、戦略へは元の受信時刻まで配送しません。
執行モデル名は `exchange_timeline_fifo_v2` です。

## 設定と時間軸

```rust,ignore
let venue = PaperTradingVenue::with_latency(engine, latency)?
    .with_simulation_config(SimulationConfig { lookahead_ms: 10_000 })?;
let runtime = BitbankBotBuilder::new(strategy, venue).add_pair("btc_jpy").spawn();
```

`SimulationConfig` の既定待機時間は10,000msです。非負整数を指定し、実行中には変更できません。
待機時間、時計差δ、発注・取消・照会・通知の遅延は独立設定です。
10秒待つことは欠損がない保証ではなく、「この時間内に必要な市場情報が届く」という仮定です。

| 時刻 | 意味 |
| --- | --- |
| captureの `received_at_micros` | 元のローカル受信時刻。マージ順を保持し、msへ切り捨てる |
| `ctx.received_at_ms()` | その入力の元の受信時刻。API待機後も変化しない |
| `ctx.now_ms()` | 現在のローカル仮想時刻。API待機中にも進む |
| 板timestamp・公開約定executed_at | 取引所の発生時刻E |
| 内部市場の実行期限 | E − δ。`PaperTimeModel` は E = L + δ |
| Paperの進行上限 | 実時間 − lookahead_ms |
| `recorded_at_ms` | ログを書いた実時計。執行判定に使わない |

エンジンは `PaperClock::Replay { now_ms: 0 }` で作成できます。
`Realtime` で作ったエンジンも、通常のPaper起動時にはドライバがReplay時計へ切り替えます。
初期口座・初期注文は利用者が与える開始条件です。未来の板や注文を初期状態へ混ぜないでください。
時計差δは固定の仮定であり、自動推定・ドリフト補正は行いません。
時刻の欠落、加減算overflow、換算後の発生時刻が受信より未来になる入力はエラーです。

実行順は、内部市場登録、注文・取消の有効化、約定・取消の確定、照会採取、応答・通知、戦略配送です。
同msの板はsequence、約定はtransaction IDで安定化します。戦略への同ms入力は元の入力順です。
注文の開始・取消と同じmsの約定は除外します。封鎖済みmsで新たに取消を要求した場合は次のmsに有効化します。
その取消期限もスケジューラで処理し、次の市場通知を待たず資金を解放します。

## 板の復元と戦略の窓口

生のsnapshot・差分を内部用と戦略用の別converterへ渡します。
内部用は発生時刻順、戦略用は受信順です。受信順で作った板snapshotの時刻だけを並べ替えません。
公開約定は内部で個々の成立時刻へ分解し、戦略へは元の通知単位・配列順で配送します。

bitbankの板sequenceは昇順ですが連番ではないため、数字の飛びだけを欠損とみなしません。
発生時刻順との矛盾、同じsequenceの内容衝突はエラーです。同一内容の再通知は無視します。
snapshot以前の内部差分は後から遡及適用せず、板不足として評価履歴へ残します。
後で正常なsnapshotが来ても、不確かな評価だった履歴は消しません。

注文到着時の内部板でpost-onlyと先行数量を判定します。戦略が判断時に見た板を固定しません。
戦略へ渡す `ctx.venue()` は注文・取消・照会と通知済み口座の窓口です。
内部engineは取得できず、内部診断・時計操作・設定変更も利用できません。
診断は実行基盤の `BotRunner::paper_diagnostics()`、または
`PaperTradingVenue::with_diagnostics(engine)` が返す所有者用ハンドルで読み取ります。
診断snapshotは内部状態を変更しません。ハンドルを戦略へ渡さないでください。

独自入力のPaperにも `SimulationDriver::admit_bitbank` / `admit_event` と `advance_to` を使います。
`DepthUpdated` を直接投入する場合は独立snapshotに限定し、差分由来の板には生メッセージ経路を使ってください。
`BotRunner::process` / `submit_replay` とengineの手動watermark操作は低水準kernelの検証用であり、
通常のPaper・バックテストの入力経路としては使いません。

## 板・FIFOの数値表現と差分更新

価格は`Price`（小数8桁のi128）、数量は`Quantity`、手数料率は`Rate`（ともに小数8桁のi64）、
売買代金・手数料・各資産の残高は`Amount`（小数24桁のi128）で保持します。
取引所の十進文字列とJSON数値は正確に解析し、丸め・浮動小数点への変換を行いません。
これはペア固有のtick/lot検証を代替するものではありません。
価格の正の上限は約1.7 × 10^30、数量・率は92,233,720,368.54775807、金額・残高は約170兆です。
精度超過とoverflowは`NumericError`として拒否し、暗黙の切り捨てはしません。
約定と取消は口座更新を検証してから確定し、数値エラーで一部だけの資金移動やFIFO消化を残しません。

`Depth`も`BTreeMap<Price, Quantity>`を返し、最良気配・スプレッドに型変換はありません。
`MarketDepthSnapshot::new`には同じ型の板を渡します。対数などの近似分析に限り明示的にf64へ変換します。
旧公開APIとの変換層はありません。型の使い方・換算式・JSON表現は[数値型](numeric-types.md)を参照してください。

通常の差分更新は変更価格だけをFIFOへ適用し、表示減少から先行数量を消費しない契約を維持します。
同じ接続の全体板も、直前の復元板との差分を適用します。
接続切替、更新を取りこぼした入力では全体から再同期します。
`MarketDepthSnapshot::changes_since` はシーケンス番号の連番を仮定せず、実際の更新元の同一性を確認します。
板snapshotは小さな価格区画をArcで共有し、後続更新で過去のsnapshotを変更しません。
最良気配・価格ごとの数量・`levels`走査は全板コピーを行いません。
`Depth::asks` / `bids`のBTreeMapは初めて要求されたときに実体化してsnapshot内に保持します。
独立に作ったsnapshot同士は内容が等しくても差分関係にしません。

## 注文・口座の遅延

`PaperLatencyConfig` のplacement・cancellation・orders_query・balances_queryには
`PaperOperationLatency { entry_ms, response_ms }` を設定します。全て非負整数、既定0です。

- 発注・取消は要求の最初のpoll時に登録し、entry_ms後に内部取引所へ到着します。
- 受付・拒否の応答は有効化からresponse_ms後に返ります。
- 照会は到着時の注文・残高を別々に凍結し、応答時の最新状態へ差し替えません。
- 注文一覧と残高の採取は原子的ではなく、古い照会結果を通知用キャッシュへ書き戻しません。
- 約定・取消確定からnotification_ms後に口座へ通知します。公開約定の戦略向け配送より先に届くこともあります。

通知用キャッシュは `known_account_snapshot()` です。通知は内部状態遷移の一貫したprefixを公開するため、
遅い先行通知が後続通知も遅らせる場合があります。照会はこのキャッシュと独立して内部状態を採取します。
取引所で有効化した時点で資金を判定・拘束し、送信中の要求のためのローカル予約はしません。
同じ資金を見て送った競合要求は一方が拒否され得ます。Futureのdropは送信済み要求を取り消しません。

## 終了・異常・記録

バックテストは、入力sourceごとの最終受信時刻の最小値からlookahead_msを引いた時刻まで評価します。
明示終了時刻を指定した場合は、その小さいほうが終了境界です。
末尾の先読み入力は内部再構成のために使い、戦略へは配送しません。EOFで評価を延長しません。
終了時の未応答操作・未通知状態・残注文を強制完了・取消せず保存します。

遅延上限超過、処理済み境界への未知の遅着、内容衝突、観測した切断・欠損では評価を無効化し、
新規注文・内部約定・資金確定を止めます。再接続や新しい板では解除しません。
`recovery: true` は異常を保持したまま市場配送を継続する設定で、戦略のエラーは握りつぶしません。
Paperではコールバック内で時刻を採取し、チャネル満杯を入力欠損として扱います。
観測できた再接続も異常にしますが、依存ライブラリ内の短時間の再接続や未観測の欠損の完全検出は保証しません。

`ReplayReport.simulation` に待機設定・入力受信範囲・評価終了境界・保留入力数・異常理由を残します。
`settlement` には内部確定境界・執行モデル・残操作・未確定フラグを残します。
watermarkは新方式ではスケジューラが管理する内部処理境界です。戦略が受信後に手動で進める必要はありません。
`max_trade_delay_ms` は低水準kernelの旧受信方式の設定で、共通ドライバでは使用しません。

`simulation_model`、`simulation_market`、`input_registered`、`paper_operation_*`、
`paper_local_state`、`replay_outcome` / `simulation_outcome` を既存レコーダーへ保存します。
市場発生・元受信・仮想処理・実記録時刻と入力IDを区別します。
正常shutdown・失敗では確定状態を保存します。Dropによる強制停止では保証しません。
レコーダーは入力の生ログの代替ではありません。終了後に `finish()` と `status()` も確認してください。

## FIFOとモデルの限界

板は初期化済みの両side、正の価格・数量、best bid < best askが必要です。
可視範囲外・板なし・不正板は `ExecutionModelUnavailable` です。
Normal modeでcrossするpost-onlyは注文全体を拒否し、post-onlyなしのcrossはtaker未対応として拒否します。
`assume_normal_mode` の既定はtrueで、モードを観測した場合は観測値を優先します。

同価格の外部先行数量と自注文は共有FIFOです。公開約定数量を先行数量から消化し、その超過だけ自注文へ配分します。
板数量の減少だけでは順位を繰り上げず、板と公開約定で数量を二重消化しません。
観測数量の増加は既存注文の後ろへ追加します。価格を通過した公開約定の配分も公開数量を上限にします。
代金とmaker手数料は注文価格で計算します。

真の注文順位、非表示数量、市場インパクト、自分の注文による他者の行動変化は再現しません。
同ms・別チャネル間の順序には仮定が残ります。損益が必ず保守的になる保証もありません。
重複検出履歴とFIFO価格履歴は実行期間中保持するため、長期評価ではメモリ量を監視してください。
キュー履歴と記録量は [queue-history.md](queue-history.md)、[execution-recording.md](execution-recording.md) を参照してください。
EOFや板の進行だけから欠損のない真の取引所状態が分かる、というモデルではありません。
