//! 保存済みの実行記録だけを集計する。APIへは接続しない。
use bitbankutil_rs::execution_recording::read_records;
use std::{collections::BTreeMap, fs::File, io::BufReader};

#[derive(Default)]
struct Distribution {
    count: usize,
    samples: Vec<u64>,
    buckets: [usize; 6],
}
impl Distribution {
    fn add(&mut self, elapsed: Option<u64>) {
        self.count += 1;
        if let Some(us) = elapsed {
            self.samples.push(us);
            let bucket =
                [10_000, 50_000, 100_000, 250_000, 1_000_000].partition_point(|limit| us >= *limit);
            self.buckets[bucket] += 1;
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("JSONLファイルを指定してください")?;
    let records = read_records(BufReader::new(File::open(path)?))?;
    let operations: BTreeMap<_, _> = records
        .iter()
        .filter(|r| r.kind == "operation_started")
        .map(|r| {
            (
                (r.run_id.clone(), r.operation_id),
                r.data["operation"].as_str().unwrap_or("unknown").to_owned(),
            )
        })
        .collect();
    let mut groups: BTreeMap<_, Distribution> = BTreeMap::new();
    let mut unfinished = BTreeMap::new();
    for r in &records {
        if r.version != 2 {
            return Err(format!("未対応の記録形式: {}", r.version).into());
        }
        let (level, started) = match r.kind.as_str() {
            "operation_started" => ("operation", true),
            "operation_finished" => ("operation", false),
            "http_request_started" => ("request", true),
            "http_request_finished" => ("request", false),
            "http_started" => ("attempt", true),
            "http_finished" => ("attempt", false),
            _ => continue,
        };
        let key = (
            r.run_id.clone(),
            level,
            r.operation_id,
            r.data["request_id"].as_u64(),
            r.data["attempt"].as_u64(),
        );
        if started {
            unfinished.insert(key, r.sequence);
            continue;
        }
        unfinished.remove(&key);
        let endpoint = if level == "operation" {
            "-".to_owned()
        } else {
            format!(
                "{} {}",
                r.data["method"].as_str().unwrap_or("?"),
                r.data["endpoint"].as_str().unwrap_or("?")
            )
        };
        let operation = operations
            .get(&(r.run_id.clone(), r.operation_id))
            .cloned()
            .unwrap_or_else(|| endpoint.clone());
        let outcome = if r.data["timeout"] == true {
            "timeout"
        } else if r.data["success"] == true {
            "success"
        } else {
            "failure"
        };
        let stage = r.data["stage"].as_str().unwrap_or("-").to_owned();
        groups
            .entry((r.run_id.clone(), level, operation, endpoint, outcome, stage))
            .or_default()
            .add(r.data["timing"]["elapsed_us"].as_u64());
    }
    println!(
        "各行は実行・計測階層・操作・エンドポイント・結果・失敗段階別。異なる階層は合算しません。"
    );
    println!("ヒストグラム(ms): [0,10), [10,50), [50,100), [100,250), [250,1000), [1000,+inf)");
    for (key, mut d) in groups {
        d.samples.sort_unstable();
        let summary = if d.samples.is_empty() {
            "時刻なし".to_owned()
        } else {
            let ms = |us: u64| us as f64 / 1000.0;
            format!(
                "min={:.3} sample_median={:.3} max={:.3} ms",
                ms(d.samples[0]),
                ms(d.samples[(d.samples.len() - 1) / 2]),
                ms(*d.samples.last().unwrap())
            )
        };
        println!(
            "{key:?}: count={} timed={} {summary} histogram={:?}",
            d.count,
            d.samples.len(),
            d.buckets
        );
    }
    println!(
        "開始だけ残った記録（結果不明、成功・失敗の件数から除外）: {}",
        unfinished.len()
    );
    println!("分布はこの標本内の記述統計です。少量標本から母集団のp95等は推定しません。");
    Ok(())
}
