# オフライン戦略実行とバックテスト

Paperとバックテストは `SimulationDriver` を共有します。内部市場は発生時刻、
戦略への配送は元の受信時刻です。設定・約定・異常処理は [paper-execution.md](paper-execution.md) を参照してください。

```rust,ignore
let venue = PaperTradingVenue::new(engine)
    .with_simulation_config(SimulationConfig { lookahead_ms: 10_000 })?;
let mut runner = BotRunner::new(strategy, venue);
// Input files must remain immutable between preflight and execution.
let metadata = StreamingReplayMetadata::scan(ReplayInput::open(manifest)?)?;
let report = run_replay(&mut runner, ReplayInput::open(manifest)?, &metadata, false, &cancel, custom).await;
```

`run_replay_with_options` の `end_time_ms` はローカル仮想時刻です。
実際の終了は「指定終了」と「各sourceの最終受信時刻の最小値 − 待機時間」の小さいほうです。
入力sourceは全て評価に必要なものとして扱います。不要なsourceは入力から除外してください。
EOFだけで市場のない区間や予約期限まで評価を延長しません。入力が空なら時計も進みません。
同msの入力は元のus単位のマージ順を保持します。

sourceごとの安全な末尾と購読・入力整合性は `StreamingReplayMetadata::scan` または
同一の事前検査での `observe` により確定します。原本を再度読むので2-passですが、一時JSONLを
書き出しません。`run_replay` は別スレッドで lookahead-safe な `CompiledBlock` を逐次生成し、
使い終えたブロックを解放します。raw decode とsimulationを別経路にした旧方式は削除しました。
customイベントは `Clone + Sync`、供給iterator/custom decoderは `Send + 'static` が必要です。
変換は先読みするため、`custom` は戦略状態に依存しない関数にしてください。

事前検査後の入力変更は許可しません。manifest、入力hash、compile設定、decoder/buildの識別は
callerで管理してください。`close_window` は元のcoverageを確認したcallerが、sourceごとに
同じhorizonの時計専用markerを末尾に付ける場合だけ使います。coverageの証明を作るAPIではありません。
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

## Compiledブロックの逐次生成とディスク再利用

simulation実行は常にcompile済みイベントを消費します。供給元だけを切り替えます。

- `BlockCompiler`: converterの状態とexchange順heapをブロック間で継続し、未読受信時刻−lookaheadより厳密に古い市場イベントを確定する。同一ms境界や遅着で独立ソート・再初期化しない。
- `CompiledBlockReader`: 保存済みcompiledを1ブロックずつ復元する。raw decoder/市場変換は再実行しない。展開・デシリアライズは必要。
- `CompiledBlockCache::iter()`: 同じブロックを全件RAMに保持する任意の供給元。

いずれも `run_compiled_block_source` の同じsimulation consumerを使います。
単発は `run_prefetched_compiled`、複数ケースは `run_shared_blocks` で供給と実行を重ねられます。

```rust,ignore
let config = SimulationConfig { lookahead_ms: 180_000 };
let compiler = BlockCompiler::new(input, config, time_model, custom_decoder, BlockOptions::default())?;
// 逐次生成。戦略・venue・注文・口座・recorderはfactoryごとに新規生成。
let results = run_shared_blocks(compiler, factories, &metadata, options, &cancel);

// 別の実行で事前生成する場合。identityは入力hash・compile設定・decoder/buildを含める。
write_compiled_blocks(new_cache_path, identity, compiler)?;
// 再利用。全期間をRAMに保持せず、同じconsumerへ供給する。
let blocks = CompiledBlockReader::open(cache_path, identity)?;
let results = run_shared_blocks(blocks, fresh_factories, &metadata, options, &cancel);
// results.source_error と全caseの ReplayReport.error/interrupted/incomplete を検査する。
```

`write_compiled_blocks` は新規ファイルだけ作り、既存ファイルを上書きしません。失敗時は削除します。
永続cacheを公開するcallerは専用partialパスへ生成し、完成後にatomicに公開してください。
formatはversion付き・checksum付きzstdフレーム（内部の型付きイベントはserde JSON）。
各ブロックの先頭に板checkpoint、後続にはcompile済み差分を保存し、復元でもCOW共有を保持します。
未完成footer、checksum、範囲不正、identity不一致はエラーです。cross-version互換は保証しません。
cache化するcustomイベントにはさらに `Serialize + DeserializeOwned` が必要です。
metadataのpreflight/coverageとcache identityの管理はcallerの責任です。

既定4096 records/block、compile確定前の入力上限65536 records、待機queueはconsumerごと1 blockです。
上限を超えるlookaheadでは明示的に失敗します。供給側は最も遅いcaseでbackpressureが掛かります。
検証履歴・1通知のサイズ・strategy/simulation/記録の状態は別であり、プロセス全体の固定RAM上限ではありません。
case数ぶんのスレッドと状態を使うため、runner側で同時ケース数を制限してください。

非simulationの受信順callback検査には `run_capture_replay` / `run_capture_replay_with_options` を使います。
simulation venueはこれらに渡せません。既存 `run_replay` 呼び出しはmetadata引数を追加し、
custom型を共有可能にするか、非simulation用途を `run_capture_replay` へ移行してください。

[compiled_blocks example](../examples/compiled_blocks.rs) で逐次生成・保存・再利用を試せます。

## 反復backtest

同じ入力で複数の戦略・パラメータを評価する場合は、`PreparedReplay`で検証とJSON解析を一度だけ行い、
`compile`で板の復元と重複検証も事前計算できます。入力・復元板をRAMに保持するため、
逐次compiledの`run_replay`よりメモリを使います。省メモリで再利用する場合は次節のディスクブロックを使います。

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

## 共有compiled入力で複数ケースを並列実行する

`examples/shared_replay.rs` は1回のprepare/compileを、上限個のスコープ付きOSスレッドで共有する例です。
各スレッドのcurrent-thread Tokio runtimeがケースを順に取得し、ケースごと（反復ごと）に戦略、
`BotRunner`、venue、残高、注文・FIFOキュー、recorderを作り直します。
`CompiledReplay`をcloneせず借用し、全スレッドの終了まで`PreparedReplay`を保持します。
customイベントも共有可能な型（この例では`serde_json::Value`）にしてください。

```sh
cargo run --release --example shared_replay -- 2 8 100
# workers cases iterations manifest first_case
cargo build --locked --release --example shared_replay
python3 scripts/bench_shared_replay.py --workers 2 --cases 8 --iterations 100
```

例の`run_cases`は投入順で結果を返し、`report.error`、`incomplete`、`interrupted`をケースごとに残します。
他ケースの失敗で全体を打ち切りません。`Case.fail`と`response_ms`は戦略失敗と遅延の検証にも使えます。
共有の`Arc<AtomicBool>`を別スレッド等からtrueにすると、実行中のリプレイと未開始のケースを中断状態にします。
`cancel_on_event`はイベント処理中にキャンセルを起こす検証用設定です。
キャンセルは協調的なので、戦略の同期処理が戻らない場合の強制停止はできません。
worker自体のpanicは例全体の失敗です。通常のケースエラーとは区別します。

比較用fixture `fixtures/shared-replay` は元の`market-data-v1`の切断前入力と、
両sourceで評価終了+30msまで収録するheartbeatからなる合成入力です。
最良買気配+0.1に注文し、部分約定、取消、未決注文を発生させます。
テストはケースの入力ID・入力順、判断、約定、最終残高・注文、終了reportを含む記録を比較します。
壁時計の記録時刻、操作の実時間計測、ケースをまたいで採番される診断用queue_stream_idのみ除外します。
2/4/8 workerと逆順投入の一致、ケース失敗の独立性、不完全終了、実行中と投入前のキャンセルも確認します。

### 比較方法と測定結果

Linux専用の`python3 scripts/bench_shared_replay.py`は外部Pythonライブラリを使いません。
同じrelease実行ファイル・manifest・ケースID・反復回数・記録方式で、共有compiledの逐次実行、
上限個の別プロセスへケースを等分する方式、共有compiledの並列実行を比較します。
各方式4回、逐次→プロセス→並列→並列→プロセス→逐次を2巡し、各結果の一致を確認します。
`--manifest`で実際の市場入力へ差し替えられます（戦略の注文対象はbtc_jpy）。
この例のデフォルトは回帰確認用の小さい合成入力であり、大規模履歴でのprepare/compile共有の効果は測れません。

各行はprepare、compile、ケース実行のプロセスごとのms配列、全体の壁時計ms、
1ms間隔で子プロセスのRSSを合算した観測ピークKiBをJSON出力します。
壁時計はプロセス起動、結果のJSON出力と終了待ちを含み、Python側の結果読取・比較を除きます。
RSSはサンプリングのため瞬間的ピークを見逃し、共有ページの重複計上があります。Python親のメモリは含みません。
プロセスごとのフェーズ時間の合計は並行する仕事量であり、全体の壁時計とは異なります。
反復は同一ケースを新しい状態で実行して短いfixtureの実行負荷を増やすもので、入力の長さを増やしません。
ログはメモリに保持し、ケースごとの最終反復の結果を出力・比較します。記録コストもケース実行時間に含みます。

2026-11-20、Linux x86_64、AMD Ryzen 5 8600G（6コア/12スレッド）、
rustc 1.99.0、2 worker、8ケース×100反復、11入力レコードで測定した4試行の中央値:

| 方式 | prepare合計ms | compile合計ms | ケース実行合計ms | 全体ms | 観測ピークRSS KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 共有逐次 | 0.518 | 0.042 | 718.766 | 730.013 | 14106 |
| プロセス分割 | 1.058 | 0.075 | 724.537 | 370.278 | 19718 |
| 共有並列 | 0.527 | 0.044 | 366.424 | 377.396 | 15662 |

生データは[benchmarks/shared-replay.jsonl](benchmarks/shared-replay.jsonl)にあります。
この小さいfixtureでは共有並列の壁時計は逐次の約52%でしたが、プロセス分割より速い結果ではありません。
共有並列の観測RSSはプロセス分割より小さくなりました。
下流での73.66秒のワークロードとは異なるため、その改善を主張する結果ではありません。
大規模な実入力と実戦略で再測定してからAPIを判断できるようにし、今回は共通の並列実行APIを追加していません。
EOF、不完全終了、キャンセルの意味は既存の`CompiledReplay::run`に従います。
EOFで注文の取消や残りの操作を強制的に完了させません。
