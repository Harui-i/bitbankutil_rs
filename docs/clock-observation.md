# 時計差とWS到着遅延を調べる

`clock_observation`は、通常の発注・取消の時刻から時計差の区間を求め、WSの到着遅延を調べるための観測を提供します。追加のAPI通信、時計設定の変更、自動的な注文時刻の補正は行いません。

## 記録する

Private APIクライアント（または`BitbankTradingVenue`）の`with_recorder`と、`BitbankBotBuilder::websocket_observation(recorder.clone())`に同じ`ExecutionRecorder`を渡してください。これでHTTPとWSを同じ`run_id`へ保存できます。WS単独利用では`run_websocket(pair, options, config, sender, Some(recorder.clone()))`を使います。WS観測をしない場合は`None`です。

```rust,no_run
use bitbankutil_rs::{
    bitbank_private::BitbankPrivateApiClient,
    execution_recording::{ExecutionRecorder, JsonlSink},
    websocket_handler::run_websocket,
};
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let recorder = ExecutionRecorder::new("unique-run-id", serde_json::json!({"host":"bot-01"}), JsonlSink::open("execution.jsonl")?);
let client = BitbankPrivateApiClient::new(std::env::var("BITBANK_API_KEY")?, std::env::var("BITBANK_API_SECRET")?, None)
    .with_recorder(recorder.clone());
let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
let ws = tokio::spawn(run_websocket("btc_jpy".into(), vec![], Default::default(), sender, Some(recorder.clone())));
// receiverを消費しつつclientで通常の注文操作を行う。
let _ = (&client, receiver.recv().await);
ws.abort();
let _ = ws.await;
recorder.finish()?;
assert!(recorder.status().error.is_none());
# Ok(())
# }
```

WS観測は明示的に指定した場合だけ保存します。Financial/Diagnosticの双方で保存でき、Disabledでは保存しません。終了時はWSとHTTPの処理を止めてから`finish()`し、`status()`の記録失敗・欠損も確認してください。複数プロセスを同じrun_idに混ぜないでください。

## 記録の意味

`ClockObservation`の`unix_us`はローカルUTCのUnixマイクロ秒、`monotonic_us`はプロセス共通起点からの経過マイクロ秒です。単位は精度の保証ではなく、2時計の読み取りは同時ではありません。別プロセスの単調時計は比較できません。

HTTPの`http_finished.data.request_clock`は各試行の開始記録より前、`response_clock`は本文受信後の応答ハンドラー冒頭で採取します。後者は観測用JSON解析・基盤ハンドラーの実行より前です。要求の組立・接続待ちなども区間に含まれ、パケット送受信時刻ではありません。`response_clock`は応答が届かない場合nullです。既存の`timing.completed_at_unix_us`は応答処理後の別の観測点です。注文ID・`ordered_at`・`canceled_at`は既存の許可フィールド抽出で保存します。

`ws_clock_observed`はJSONコールバック冒頭の`received_clock`、観測ID、ストリームID、受信順、ペア・チャンネル、`exchange_times`を持ちます。取引所時刻は`field`で区別します。約定は各`executed_at`、板差分は`t`、その他は`timestamp`です。約定ID・板sequenceも存在する場合に保存します。複数約定は同じ受信時刻を共有します。不正・未対応メッセージは`valid: false`で、分析から除外します。応答全文・認証情報は保存しません。

受信時刻は基盤ライブラリのJSON解析後、bbutilのデコード・タスク起動・チャネル送信より前です。bot側のイベント登録時刻`received_at_ms`とは異なります。ストリームIDは`run_websocket`呼び出し単位であり、基盤ライブラリの自動再接続を識別する接続IDではありません。自動再接続があっても受信順は同じストリーム内で継続します。同期sinkの保存処理は後続の受信を遅らせ得ます。

## 時計差の区間

符号は`Δ = 取引所時計 − ローカル時計`です。要求開始をL₀、応答観測をL₁、その要求で生成された取引所時刻をEとすると、`E − L₁ ≤ Δ ≤ E − L₀`です。ミリ秒の量子化への余裕として両側を1ms広げます。取引所時計の誤差や内部の時刻付与位置まで保証する余裕ではありません。

例えばL₀=0ms、E=50ms、L₁=80msなら、余裕込みの時計差は−31ms〜+51msです。上り・下りの遅延対称性を仮定せずに計算できます。

対象は成功した単一試行のPOST発注の`ordered_at`と、取消済み状態を返す単一取消の`canceled_at`です。取得APIの過去時刻、取消応答の`ordered_at`、失敗・再試行・結果不明・時刻欠損は自動推定に使いません。一括取消は初版の自動推定対象外です。取消時刻はその要求で新たに取消が発生したという前提で使います。重複操作等で過去の取消時刻が返る場合は対象から外してください。

## 保存後の分析

同じプロセス・実行の完結したJSONLを指定します。FROM/TOはローカルUTCのUnixマイクロ秒で、短い分析期間を明示します。

```sh
cargo run --example analyze_clock -- execution.jsonl 1794000000000000 1794000060000000 1000
```

最後の引数はUTC差と単調時計差の不一致の許容マイクロ秒で、既定1000です。試行単体で超える観測は除外し、採用したHTTP・WS観測間でも超える場合は期間全体の共通区間を使いません。sequence欠損・重複、run混在、`run_finished`欠損はエラーです。時計変化が観測間で相殺された場合など、すべての時計補正を検出できるわけではありません。

各HTTPの区間と期間内の共通区間を表示します。共通部分が空なら`contradiction=true`で、中点や無理に狭めた値を出しません。指定期間内で時計差が一定であるという仮定が必要です。共通区間は期間外へ適用しません。

WSには見かけの差`R − W`と、利用可能なら`R − W + Δ`による到着遅延区間を表示します。負の遅延も隠さず、時計基準や推定の前提を確認する材料にします。Private API時刻とWS時刻の時計基準が一致することは未確認です。観測チャンネルと時刻フィールドを区別して評価してください。

約定時刻、ticker更新時刻、板の配信時刻は意味が異なります。評価対象はそれぞれの時刻付与点からJSONコールバックまでの合計遅延です。配信待ち・通信・受信処理の内訳は分離できません。
