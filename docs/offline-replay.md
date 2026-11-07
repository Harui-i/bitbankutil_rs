# オフライン戦略実行とバックテスト

Paperとバックテストは `SimulationDriver` を共有します。内部市場は発生時刻、
戦略への配送は元の受信時刻です。設定・約定・異常処理は [paper-execution.md](paper-execution.md) を参照してください。

```rust,ignore
let venue = PaperTradingVenue::new(engine)
    .with_simulation_config(SimulationConfig { lookahead_ms: 10_000 })?;
let mut runner = BotRunner::new(strategy, venue);
let report = run_replay(&mut runner, ReplayInput::open(manifest)?, false, &cancel, custom).await;
```

`run_replay_with_options` の `end_time_ms` はローカル仮想時刻です。
実際の終了は「指定終了」と「各sourceの最終受信時刻の最小値 − 待機時間」の小さいほうです。
入力sourceは全て評価に必要なものとして扱います。不要なsourceは入力から除外してください。
EOFだけで市場のない区間や予約期限まで評価を延長しません。入力が空なら時計も進みません。
同msの入力は元のus単位のマージ順を保持します。

sourceごとの末尾を知るため、最初に入力を検証し、一時JSONLへ退避する2-pass方式です。
全入力をRAMに保持せず、実行中は待機窓の入力を読み込みます。一時ファイルは終了時に削除します。
入力検証中の中断・形式エラーでは戦略を開始しません。書き込み可能な一時ディレクトリと入力相当の空き容量が必要です。
`records` と `last_input` は戦略へ配送した数ではなく、先読みとして取り込んだ入力の範囲です。

戦略は `ctx.now_ms()` と `schedule_at` / `schedule_after` を使用します。
API待機中も市場・操作期限は進み、戦略ハンドラーは1件ずつ配送されます。
`ctx.received_at_ms()` は配送が滞留しても元の受信時刻のままです。
派生イベントはFIFOで、先行ハンドラーが完了した後の滞留配送より先に処理します。
無限の派生・同時刻予約・配送滞留は既存の上限でエラーにします。

他取引所・カスタム入力は元の受信時刻で配送します。`custom` は変換だけを行い、
現在の戦略状態・実時計・直接のAPI応答に依存しないようにしてください。
戦略からの実時間API呼び出し、任意のTokio sleep/interval、独立タスクの時計は仮想化しません。
並行発注は登録順を固定してください。独立タスクの起床順に依存する戦略の決定性は保証しません。

`BotRunner::process` / `submit_replay` は低水準の受信順kernelドライバです。
新しいPaper・バックテストには `run_replay` または `SimulationDriver` を使用してください。

## 反復backtest

同じ入力で複数の戦略・パラメータを評価する場合は、`PreparedReplay`で検証とJSON解析を一度だけ行い、
`compile`で板の復元と重複検証も事前計算できます。入力・復元板をRAMに保持するため、
逐次読み込みの`run_replay`よりメモリを使います。ディスクへのキャッシュ保存は行いません。

```rust,ignore
let prepared = PreparedReplay::new(ReplayInput::open(manifest)?)?;
let compiled = prepared.compile(
    SimulationConfig { lookahead_ms: 60_000 },
    PaperTimeModel::FixedOffset { exchange_minus_local_ms: 0 },
    |_| Ok(None::<std::convert::Infallible>),
)?;
// 各回で戦略・PaperTradingVenue・BotRunnerを新しく作る。
let report = compiled.run(&mut runner, ReplayOptions::default(), &cancel).await;
```

準備時間と各回の実行時間は別々に計測してください。残高・注文・手数料・約定待ち行列・戦略ハンドラーは
各回で再実行します。時計差と先読み幅はcompile時と一致する必要があります。
custom変換はcompile時だけ呼び出され、その結果を共有します。毎回変換したい場合は`prepared.run`を使います。
復元板は変更した小さな区画だけをコピーし、過去のsnapshotを保持します。
受信順と発生時刻順の板は別々に復元し、戦略から未来の板が見えない契約を保ちます。
接続切断や市場不整合の扱い、sourceごとの安全な終端は`run_replay`と共通です。
compileで見つかった市場の時系列エラーも実行時の対応する境界で停止し、未来のエラーを先に適用しません。
準備時のJSON形式・入力検証エラーは`io::Error`として返します。

best_mmを同じデータで3回実行する例（APIキー不要）:

```sh
cargo run --release --example best_mm -- --backtest \
  xrp_jpy 0.001 1000 10 1000 500 1000000 60000 /path/to/manifest.json 3
```

最初のJSON出力に準備・compile時間、その後に各回の実行時間と残高・注文・`incomplete`を出力します。
backtest時は戦略のinfoログを抑制します。市場入力に切断がある場合は`recovery=true`で継続しますが、
市場は不完全として新規注文・約定確定を停止します。`incomplete=true`を正常な全期間評価として扱わないでください。

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
## 異常と終了結果

`recovery: false` は観測した切断で止まります。trueなら異常を残して配送を継続します。
どちらも新規注文と約定確定は停止し、再接続では復旧しません。
再接続の購読集合が一意に一致する場合だけ `gaps.end` を関連付けます。
時刻・内容・sequence・変換のエラーと戦略エラーはrecoveryでも失敗です。

`ReplayReport` の `error`、`interrupted`、`incomplete` と `simulation`、`settlement` を保存してください。
正常な末尾切り捨ては単独ではincompleteにしません。残操作・未通知・モデル異常があればincompleteです。
返却時にハンドラーFutureを破棄しますが、送信済み要求と残注文は取り消しません。
最後の口座を読む場合は `runner.paper_diagnostics()` のsnapshotを使い、終了後の照会で期限を強制実行しないでください。
レコーダーのfinishは自動では行いません。

## サンプルとテスト

```sh
cargo run --example offline_replay -- /tmp/replay.jsonl
cargo test --locked --test simulation
```

サンプルはAPIキー・ネットワーク・実時間sleepを使わない合成fixtureです。
末尾に先読みの補助入力を与え、最終注文を残したまま口座と結果を保存します。
テストはpost-onlyの新旧の差、二重の板復元、通知・照会、同ms取消、時計差、遅着・内容衝突、
最短sourceの終了境界、仮想実時間のPaperとバックテストの一致を確認します。
実履歴での損益改善を保証するテストではありません。

## 実験: 同期イベント・コマンド戦略

`EventStrategy::on_event`は同期的に`Request`を出力する。口座照会・発注・取消の結果は、要求IDを持つ後続の`StrategyEvent::Response`として受け取る。`true`を返す間は新しい市場配送を保留するため、従来のasyncハンドラーの応答待ちを明示的な状態として表現できる。内部市場・注文の有効化・約定の確定は、その間も進む。

`EventRunner`は`OwnedPaperExecution`を単独所有する。市場配送ごとのFuture、共有context、waker、取引所へのMutex経由のアクセスは不要になる。注文・照会の登録と処理、採取値の凍結、市場順序、先読み、終了判定はasync版と共通。要求の登録と完了は同じではなく、設定されたentry/response遅延を維持する。

best_mmの比較実行（引数は`--backtest`と同じ）:

```sh
cargo run --release --example best_mm -- --backtest-commands \
  xrp_jpy 0.001 1000 10 1000 500 1000000 60000 MANIFEST 7
```

初回の解析・compileと反復実行時間を分けて出力する。戦略の注文判断と注文計画は通常のbest_mmと共有する。

これはバックテスト用の実験API。実時計のライブ運用、実行記録の出力、戦略からの予約イベント・派生イベントの登録はまだ提供していない。記録付きengineは構築時に拒否する。新しいランナーは反復ごとに作り直す。未完了の要求IDは一意にし、応答待ちは戦略の状態で管理する。
