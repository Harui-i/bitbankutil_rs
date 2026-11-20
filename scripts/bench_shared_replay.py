#!/usr/bin/env python3
"""Linux専用。標準ライブラリだけで3方式をABBA順に測定する。事前にreleaseビルドする。"""
import argparse
import json
import pathlib
import subprocess
import tempfile
import time


def rss_kib(pid):
    try:
        for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except FileNotFoundError:
        pass
    return 0


def measure(args, mode):
    count = min(args.workers, args.cases) if mode == "process" else 1
    files = []
    processes = []
    start = time.perf_counter()
    try:
        for index in range(count):
            first = args.cases * index // count
            cases = args.cases * (index + 1) // count - first
            output = tempfile.TemporaryFile(mode="w+")
            files.append(output)
            processes.append(subprocess.Popen([
                args.binary, str(args.workers if mode == "parallel" else 1),
                str(cases), str(args.iterations), args.manifest, str(first),
            ], stdout=output))
        peak = 0
        while True:
            peak = max(peak, sum(rss_kib(p.pid) for p in processes))
            if all(p.poll() is not None for p in processes):
                break
            time.sleep(0.001)
        wall_ms = (time.perf_counter() - start) * 1000
        outputs = []
        for process, output in zip(processes, files):
            if process.returncode:
                raise RuntimeError(f"worker exited: {process.returncode}")
            output.seek(0)
            outputs.append(json.load(output))
        results = sorted((r for o in outputs for r in o["results"]), key=lambda r: r["case"])
        if any("error" in r or r["report"]["error"] is not None for r in results):
            raise RuntimeError("case failure; inspect example output")
        metrics = {key: [o[key] for o in outputs] for key in ("prepare_ms", "compile_ms", "cases_ms")}
        metrics.update(mode=mode, wall_ms=wall_ms, sampled_peak_rss_kib=peak)
        return metrics, results
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
            process.wait()
        for output in files:
            output.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/examples/shared_replay")
    parser.add_argument("--manifest", default="fixtures/shared-replay/manifest.json")
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--cases", type=int, default=8)
    parser.add_argument("--iterations", type=int, default=1000)
    args = parser.parse_args()
    if min(args.workers, args.cases, args.iterations) < 1:
        parser.error("workers, cases, iterations must be positive")
    reference = None
    # 各方式4回。逆順も実行して実行順の影響を観察する。
    for mode in ["sequential", "process", "parallel", "parallel", "process", "sequential"] * 2:
        metrics, results = measure(args, mode)
        if reference is None:
            reference = results
        if results != reference:
            raise RuntimeError(f"case results differ: {mode}")
        print(json.dumps(metrics), flush=True)


if __name__ == "__main__":
    main()
