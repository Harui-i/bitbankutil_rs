//! cargo run --release --example shared_replay -- [workers] [cases] [iterations] [manifest] [first_case]
#[path = "shared_replay/workload.rs"]
mod workload;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};
use workload::*;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let number = |i: usize, default: usize| -> Result<usize, Box<dyn std::error::Error>> {
        Ok(args
            .get(i)
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(default))
    };
    let workers = number(1, 2)?;
    let cases = number(2, 8)?;
    let iterations = number(3, 100)?;
    let first = number(5, 0)?;
    if workers == 0 || cases == 0 || iterations == 0 {
        return Err("workers, cases, iterations must be positive".into());
    }
    let end = first.checked_add(cases).ok_or("case range overflow")?;
    let start = Instant::now();
    let prepared = prepare(args.get(4).map(String::as_str).unwrap_or(MANIFEST))?;
    let prepare_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    let compiled = compile(&prepared)?;
    let compile_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    let results = run_cases(
        &compiled,
        &(first..end).map(Case::new).collect::<Vec<_>>(),
        workers,
        iterations,
        &Arc::new(AtomicBool::new(false)),
    );
    println!(
        "{}",
        serde_json::json!({"prepare_ms":prepare_ms,"compile_ms":compile_ms,"cases_ms":start.elapsed().as_secs_f64()*1000.,"results":results})
    );
    Ok(())
}
