# Plan: reproducible measurements on two devices

Status: tooling ready, **not yet run on two devices**. The producer only ran the tooling on one
machine (both roles on loopback), which checks that the commands work and is *not* a measurement
of anything in this plan; that run is labelled as such by the tool itself. Everything below is
meant to be executed by anyone with two machines and the repository, without services of ours.
Results go into `benchmarks/RESULTS-two-devices.md` with the exact commands, hardware, OS, link
type and the commit hash; percentages are archive or volume size relative to the input (lower
is better).

## 0. How to run it

`benchmarks/two_device_bench.py` drives the whole plan with the release CLI (build it first on
both devices from the same commit: `cd tsaur && cargo build --release`).

```bash
# device A (holds the archives)
python benchmarks/two_device_bench.py prepare --work /bench            # corpora S, M, L; split 4+2; server identity; manifest
python benchmarks/two_device_bench.py serve   --work /bench --listen 192.168.1.10:7407 --allow <B's fingerprint>
# device B (receives)
python benchmarks/two_device_bench.py keygen  --work /bench            # prints B's fingerprint for A's --allow
python benchmarks/two_device_bench.py fetch   --work /bench --manifest /bench/A/manifest.json \
        --from 192.168.1.10:7407 --peer-id <A's fingerprint> --repeats 3 \
        --device-a "..." --device-b "..." --link "Ethernet 1 Gbit/s" --raw-mbps 940
```

`prepare` writes `A/manifest.json` (archive sizes and SHA-256, set ids, descriptor hashes, A's
fingerprint, commit); copy it to B by hand: that copy is the trusted channel of the trust
contract. `fetch` runs, per corpus and per repeat: fetch of the needed volumes, fetch of all
volumes, interruption at 25 / 50 / 75 % of the pieces with `--stop-after` followed by a resume,
and the join, recording the CLI's own split of connect / TLS handshake / transfer / join
seconds, peak memory of the fetching process, and the SHA-256 of every rebuilt archive against
the manifest. It writes `B/results-<time>.json` and renders `B/RESULTS-two-devices.md` (the
form in `benchmarks/TWO-DEVICE-RESULTS-TEMPLATE.md`). `--plain` on both sides measures the
unencrypted mode for comparison. `--same-machine-check` marks a run made with both roles on
one machine as a tooling check, not a measurement.

## 1. Setup

* Device A (source) and device B (receiver) on the same local network, connected once by
  Ethernet and once by Wi-Fi; note the nominal link speed and an `iperf3`-class raw throughput
  figure for reference (any tool; only the number and the command are recorded).
* One build per device from the same commit (`cargo build --release`, and `--no-default-features`
  for the lite comparison); record `tsaur --version`, `rustc --version`, OS and CPU.
* Time source: `Measure-Command` / `time` around whole commands; peak memory from the OS
  (`Get-Process` PeakWorkingSet64 sampled every 40 ms on Windows, `/usr/bin/time -v` on Linux),
  as already done for the 400 MB single-machine run.

## 2. Corpora

| Name | Content | Size | Why |
|---|---|---|---|
| S (small) | the default archive of `benchmarks/corpus` | 0.96 MB | overhead of volumes and the fixed costs of a transfer |
| M (medium) | archive of `benchmarks/corpus_versions` plus `corpus_real` | ~1.5 MB | documents with containers and JPEGs |
| L (large) | archive of a 400 MB generated file (`benchmarks` recipe from the 400 MB run) | ~130 MB | throughput, memory, resume |
| XL (optional) | 4 GB of user-chosen data (photos or a VM image), not redistributed | ~4 GB | limits of the adaptive piece size and of one-piece-per-connection |

Each corpus is packed once on A; the archive hash and the `split --json` report (set id,
descriptor hash) are copied to B by hand — that copy is the trusted channel the trust contract
assumes.

## 3. Measurements

| # | What | How | Recorded |
|---|---|---|---|
| 1 | volume overhead | `volumes split` with the adaptive default and with 64 KiB / 256 KiB / 1 MiB for 4+2, 2+1, 8+2 | total volume bytes, overhead %, padding %, split time |
| 2 | transfer throughput | A: `volumes serve --expose-lan --tls-identity --allow`; B: `volumes fetch --set --descriptor --from A --peer-id --out`; `--volumes needed` and `all` | MB/s of pieces received (bytes / wall time), pieces per second, and the CLI's own split: connect, TLS handshake, transfer, join seconds (one connection per piece, so handshake cost per piece is visible) |
| 3 | peak memory | during 2 on both devices | peak working set of `tsaur` on A and on B |
| 4 | resume after interruption | kill `fetch` on B at 25 %, 50 %, 75 % (by bytes), restart with the same command | pieces re-verified, pieces re-fetched, extra time versus an uninterrupted fetch, final `join` bit-identical |
| 5 | interruption on the serving side | stop and restart `serve` on A while B fetches | B's retries, whether B finishes without manual action after A returns |
| 6 | join cost | `volumes join` on B from the fetched volumes; compare with copying the plain archive over the same link | time, memory, bit-identical hash |
| 7 | lite versus full | repeat 2 and 6 with the lite build on B for corpus M | identical bytes; where the lite build stops (JPEG entries) |
| 8 | small versus large | 1–6 for S and L (XL optional) | fixed cost per transfer (S) versus sustained throughput (L) |
| 9 | encrypted transport | repeat 2–4 with `--tls-identity` / `--peer-id` (pinned TLS) | throughput and CPU delta versus plain TCP, handshake cost per piece (one connection per piece) |

## 4. Acceptance signals

* Every join is bit-identical to the archive on A (hash compared) — otherwise the run is a
  defect report, not a benchmark.
* Resume never fetches a piece that was already verified on disk (`pieces_received` on the
  second run equals the pieces missing at the kill point).
* Memory on B stays within one piece size plus the descriptor plus a constant, whatever the
  corpus; on A within one 1 MiB buffer per connection.
* Throughput is reported against the raw link figure so that the protocol's own overhead (one
  connection per piece, no pipelining) is visible; that number decides whether pipelining or
  parallel connections are the next optimisation.

## 5. What is deliberately not measured yet

Discovery, NAT traversal, more than two peers, public networks. Those belong to later work
packages and would need their own plans.
