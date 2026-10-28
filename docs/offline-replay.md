# オフライン戦略実行とbacktest

過去の市場データを使ったbacktestでは、逐次ランナーと市場データの再生機能を組み合わせます。

`BotRunner::new(strategy, venue)` は通信を起動しない逐次ランナーです。
`process(time_ms, input_id, event).await` の完了時には、口座更新、戦略、
`ctx.emit()` で生じた派生イベントまで完了しています。市場イベントの適用は戦略より先に1回行います。
`BotStrategy` は関連型 `Error` と `Result<(), Self::Error>` を返すハンドラーを実装します。

戦略の時刻判定には `ctx.now_ms()` を使います。通常のWebSocket実行は実時計、
直接入力では指定された仮想時計です。仮想時刻は非減少で、同時刻の複数イベントを許可します。
派生イベントは起点と同じ時刻・入力IDでFIFO処理し、次の入力を先行させません。
既定の派生件数上限は起点入力あたり10,000件で `derived_limit` から変更できます。
上限超過は、戦略がemitのエラーを無視しても実行失敗になります。
処理完了後に保持したcontextからemitしてもエラーになります。
任意のsleep、interval、実時計、独立タスクは仮想化しません。
状態変更を伴う操作はハンドラー内で順番を固定しawaitしてください。

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
`recovery: true` の検証モードだけ継続し、欠損区間を `gaps` に残します。
gapには切断した接続の購読集合も記録します。同じsourceで空でない購読集合が完全に一致し、
対応する未終了gapが1件だけの場合に、その接続開始をgapの終了として記録します。
購読配列の順序は問いません。別銘柄の接続・購読の部分一致や変更・空の購読集合・
同じ購読集合のgapが複数ある場合は、対応を断定せず未終了のまま残します。
再接続後に板が復旧しても、全体のincompleteは解除しません。
通常の入力エラーを復旧モードで握りつぶすことはありません。
`error`、`interrupted`、`incomplete` がすべて偽／空である場合だけ完全な終端到達です。
中断フラグは入力境界で確認し、実行中のハンドラーを中途で取り消しません。

`input_id` はsessionとsequenceを記録します。市場イベント、派生イベント、戦略内の操作・診断へ
同じ入力IDと実行時刻を伝播し、市場イベントに由来する派生イベントは市場イベントIDも引き継ぎます。
`recorded_at_ms` は実時計のままです。生市場データは実行記録から復元できません。
ランナーは `replay_outcome` を既存レコーダーへ記録しますが、自動でfinishしません。
最終口座の取得・残注文の取消などをawaitし、最後に `recorder.finish()` を1回呼んでください。
保存失敗は既存の `status()` とfinish/flushの戻り値で確認できます。
通常ランタイムのshutdownは入力受付を閉じて受理済み入力を処理し、処理失敗も返します。
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
