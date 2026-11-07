//! 保存済みの時計観測を分析する。ネットワーク通信は行わない。
use bitbankutil_rs::{
    clock_observation::{
        ClockObservation, ExchangeTimeObservation, OffsetInterval, private_offset,
    },
    execution_recording::read_records,
};
use std::{
    fs::File,
    io::{self, BufReader},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(args.len() == 3 || args.len() == 4) {
        return Err(
            "usage: analyze_clock FILE FROM_UNIX_US TO_UNIX_US [CLOCK_TOLERANCE_US=1000]".into(),
        );
    }
    let from: i64 = args[1].parse()?;
    let to: i64 = args[2].parse()?;
    if from > to {
        return Err("FROM must be <= TO".into());
    }
    let tolerance = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(1000);
    let records = read_records(BufReader::new(File::open(&args[0])?))?;
    let runs: std::collections::BTreeSet<_> = records.iter().map(|r| &r.run_id).collect();
    if runs.len() != 1 {
        return Err("input must contain exactly one run_id".into());
    }
    for (i, r) in records.iter().enumerate() {
        if r.sequence != i as u64 + 1 {
            return Err("record sequence is incomplete or duplicated".into());
        }
    }
    if records.last().is_none_or(|r| r.kind != "run_finished") {
        return Err("missing run_finished".into());
    }
    let mut common: Option<OffsetInterval> = None;
    let mut count = 0;
    let mut contradiction = false;
    let mut clocks = Vec::new();
    for r in &records {
        let start =
            serde_json::from_value::<ClockObservation>(r.data["request_clock"].clone()).ok();
        let end = serde_json::from_value::<ClockObservation>(r.data["response_clock"].clone()).ok();
        if let Some((s, e)) = start.zip(end) {
            if s.unix_us < from || e.unix_us > to {
                continue;
            }
            if let Some(interval) = private_offset(&records, r, tolerance) {
                println!("http sequence={} offset_us={interval:?}", r.sequence);
                count += 1;
                if !contradiction {
                    common = match common {
                        None => Some(interval),
                        Some(c) => c.intersection(interval),
                    };
                    contradiction = common.is_none();
                }
                clocks.extend([s, e]);
            }
        }
        if r.kind == "ws_clock_observed" {
            if let Ok(c) =
                serde_json::from_value::<ClockObservation>(r.data["received_clock"].clone())
            {
                if (from..=to).contains(&c.unix_us) {
                    clocks.push(c);
                }
            }
        }
    }
    // 試行の間で時計が飛んだ場合も同じ時計差で統合しない。
    clocks.sort_by_key(|c| c.monotonic_us);
    let unstable = clocks.windows(2).any(|p| {
        let wall = i128::from(p[1].unix_us) - i128::from(p[0].unix_us);
        let mono = i128::from(p[1].monotonic_us) - i128::from(p[0].monotonic_us);
        (wall - mono).abs() > i128::from(tolerance)
    });
    println!(
        "window=[{from},{to}] samples={count} contradiction={contradiction} clock_unstable={unstable} common_offset_us={:?}",
        if unstable { None } else { common }
    );
    for r in &records {
        if r.kind != "ws_clock_observed" || r.data["valid"] != true {
            continue;
        }
        let clock: ClockObservation = serde_json::from_value(r.data["received_clock"].clone())?;
        if !(from..=to).contains(&clock.unix_us) {
            continue;
        }
        let times: Vec<ExchangeTimeObservation> =
            serde_json::from_value(r.data["exchange_times"].clone())?;
        for t in times {
            let apparent = i128::from(clock.unix_us) - i128::from(t.unix_ms) * 1000;
            println!(
                "ws observation={} channel={} field={} apparent_us={apparent} delay_us={:?}",
                r.data["observation_id"],
                r.data["channel"],
                t.field,
                common
                    .filter(|_| !unstable && !contradiction)
                    .map(|c| c.arrival_delay(clock.unix_us, t.unix_ms))
            );
        }
    }
    io::Write::flush(&mut io::stdout())?;
    Ok(())
}
