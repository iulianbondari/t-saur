# Two-device transfer measurements — results form

`benchmarks/two_device_bench.py fetch` fills this form automatically (`B/RESULTS-two-devices.md`);
the same fields can be filled by hand when the tool cannot run. A run made with both roles on one
machine must say so in the status line: it checks the tooling and measures nothing in the plan.

## Status

`two devices` | `SINGLE-MACHINE TOOLING CHECK (not a measurement)`

## 1. Setup

| Field | Device A (serves) | Device B (fetches) |
|---|---|---|
| machine (CPU, RAM, disk) | | |
| OS | | |
| `tsaur --version`, commit | | |
| build (`--release`, default features or `--no-default-features`) | | |
| address used | | |

| Link | Value |
|---|---|
| type (Ethernet / Wi-Fi, nominal speed) | |
| raw throughput reference (tool, command, Mbit/s) | |
| transport (`tls` pinned / `plain`) | |
| repeats per measurement | |

## 2. Corpora (from `A/manifest.json`)

| Corpus | Input bytes | Archive bytes | Archive SHA-256 | Set id | Descriptor BLAKE3 | Piece size | Pieces | Geometry |
|---|---:|---:|---|---|---|---:|---:|---|

## 3. Fetch of the needed volumes, and of all volumes

One row per corpus, per `--volumes needed|all`, per repeat; then min / median.

| Corpus | Volumes | Repeat | Requests | Bytes received | Connect s | TLS handshake s | Transfer s | Total s | Join s | MB/s (bytes / total) | Pieces/s | Peak RSS B (MiB) | Hash OK |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|

## 4. Interruption and resume

`--stop-after k` at 25 / 50 / 75 % of the pieces, then a resume with the same command.

| Corpus | Fraction | k | First run s | Resume: re-verified | Resume: re-fetched | Resume s | Extra s vs uninterrupted | Hash OK |
|---|---:|---:|---:|---:|---:|---:|---:|---|

## 5. Peak memory on A (serving)

| Corpus | Peak RSS A (MiB) | How measured |
|---|---:|---|

## 6. Notes and anomalies

Retries, refusals (`429`/`503`), timeouts, anything that had to be repeated, and the raw JSON
file (`B/results-<time>.json`) the tables were rendered from.
