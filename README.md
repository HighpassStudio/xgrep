# xgrep

Search compressed production logs in milliseconds instead of seconds or minutes — by touching only the blocks that might match.

xgrep is an indexed search accelerator for compressed log files. After a one-time index build, repeated searches over large `.gz` corpora are **100x to 1,200x faster than zgrep** because xgrep reads only candidate blocks instead of decompressing and scanning the entire corpus.

## How it works

```bash
# First time: build an index (one-time cost)
xgrep --build-index logs/*.gz

# Every search after that is near-instant
xgrep "blk_1073741825" logs/*.gz       # specific ID: ~25ms
xgrep -F "ERROR" logs/*.gz             # broad query: ~25ms
xgrep "timeout.*connection" logs/*.gz  # regex: ~25ms
```

Traditional tools like `zgrep` decompress and scan every byte on every query. xgrep builds a sidecar index of per-block bloom filters, then uses memory-mapped I/O to touch only the blocks that might contain your search term. The OS never pages in the rest.

**Architecture:**

1. **Index build** — Decompress each `.gz`, split into 64KB blocks, build token-level bloom filters, write consolidated index
2. **Cached search** — Load bloom index (small), check each block's bloom (nanoseconds), mmap the cache, access only candidate blocks (~1% of data)
3. **Output** — Grep-compatible: line numbers, filenames, context lines, color, count mode

## Benchmark: Real Production Logs

Tested on three datasets from [LogHub](https://github.com/logpai/loghub) — real production logs from Hadoop, a supercomputer, and Spark.

### HDFS (Hadoop Distributed File System)

76 files | 743MB compressed | 7.5GB decompressed | 55.8M lines

| Query | xgrep (cached) | zgrep | Speedup |
|---|---|---|---|
| `blk_-1608999687919862906` (specific block ID) | 30ms | 27.4s | **913x** |
| `WARN` | 28ms | 25.4s | **907x** |
| `INFO` (broad — most lines) | 23ms | 28.0s | **1,217x** |

### BGL (Blue Gene/L Supercomputer)

50 files | 421MB compressed | 5.0GB decompressed | 33.2M lines

| Query | xgrep (cached) | zgrep | Speedup |
|---|---|---|---|
| `R02-M1-N0` (specific node) | 26ms | 17.4s | **655x** |
| `FATAL` | 25ms | 17.4s | **708x** |
| `INFO` (broad) | 27ms | 17.7s | **655x** |

### Spark (Distributed Compute)

3,852 files | 189MB compressed | 2.8GB decompressed | 33.2M lines

| Query | xgrep (cached) | zgrep | Speedup |
|---|---|---|---|
| `Executor` | 5.1s | 10m 2s | **118x** |
| `ERROR` | 2.7s | ~10m | **220x** |

### First query vs repeated query

| Mode | Time | Notes |
|---|---|---|
| `xgrep --build-index` | ~2 min | One-time cost |
| xgrep first query (no cache) | 2.4s | Parallel decompress + scan. Already 18x faster than zgrep. |
| xgrep repeated query (cached) | 11-30ms | Bloom skip + mmap. Only candidate blocks paged in. |
| zgrep | 44s | Full decompress + scan. Every time. |

### Key metric

**Bytes touched per query: ~0.1-1% of corpus vs 100% for zgrep.**

xgrep's cost scales with candidate data, not corpus size. As your logs grow, the advantage widens.

## JSON Mode (`-j`)

Field-aware search for NDJSON/JSONL logs. Skips blocks using bloom-indexed field-value pairs.

```bash
xgrep -j 'level=error' app.jsonl.gz
xgrep -j 'level=error status=500' logs/*.json.gz    # AND logic
xgrep -j 'http.method=post' access.jsonl.gz         # nested fields
```

### JSON Benchmark

**1M NDJSON lines, 244MB uncompressed, 22MB gzip**

| Query | `zcat \| jq` | zgrep | xgrep -j | Speedup vs jq |
|---|---|---|---|---|
| `user_id=42042` (9 matches) | 40.5s | 0.82s | **0.13s** | **319x** |
| `status=503` (111K matches) | 40.5s | 0.66s | **1.97s** | **20x** |
| `level=error status=503` (multi-clause) | 40.6s | — | **1.83s** | **22x** |

## Who this is for

Engineers who repeatedly search compressed log archives:

- **Incident investigation** — dozens of queries against the same log corpus
- **Log forensics** — searching rotated/archived `.gz` files
- **Debugging** — narrowing down by user ID, request ID, error code
- **Any workflow** where you've waited on `zgrep`

xgrep is not a universal grep replacement. It is an indexed search accelerator for cold compressed data.

## Usage

```bash
# Build index
xgrep --build-index logs/*.gz
xgrep --build-index logs/              # or pass directory

# Text search (uses cache automatically if available)
xgrep "ERROR" logs/*.gz
xgrep -F "user_id=12345" logs/*.gz     # fixed string
xgrep -i "timeout" logs/*.gz           # case insensitive
xgrep -n -C 3 "Exception" logs/*.gz    # line numbers + context
xgrep -c "WARN" logs/*.gz              # count only
xgrep -l "FATAL" logs/*.gz             # filenames only
xgrep --stats "ERROR" logs/            # show skip statistics

# JSON field search
xgrep -j 'level=error' logs/*.jsonl.gz
xgrep -j 'user_id=12345 status=500' logs/

# Plain text (no index needed — parallel SIMD search)
xgrep "TODO" src/**/*.rs
```

### Flags

| Flag | Description |
|---|---|
| `-F` | Fixed string (no regex) |
| `-E` | Extended regex |
| `-i` | Case insensitive |
| `-n` | Line numbers |
| `-c` | Count matches |
| `-l` | List matching files |
| `-q` | Quiet (exit 0 on match) |
| `-o` | Print only matched part |
| `-m N` | Stop after N matches |
| `-A/-B/-C N` | Context lines |
| `-j` | JSON field filter mode |
| `--build-index` | Build sidecar cache |
| `--stats` | Show block skip stats |
| `--include` | Glob filter |

## Install

```bash
cargo install --path .
```

## How the index works

```
logs/
  app-001.log.gz
  app-002.log.gz
  .xgrep/
    index.xgi    # bloom filters (~6% of decompressed size)
    index.xgd    # decompressed content (memory-mapped)
```

- **Bloom filters**: 4KB per 64KB block, token-level, ~3% false positive rate
- **Staleness**: File size + mtime checked every query. Stale cache ignored automatically.
- **Cache size**: ~5-6x compressed size (decompressed data + bloom index)
- **JSON mode**: Hashes `(field, value)` pairs into blooms for field-aware pruning

## License

Apache-2.0
