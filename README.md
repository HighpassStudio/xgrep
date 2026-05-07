# xgrep

Search compressed logs **100x-1,200x faster** than zgrep.

xgrep builds an index once, then only reads the blocks that might match your query.

## Install

```bash
cargo install xgrep-cli
```

## Usage

```bash
# One-time: build index (~2 min for ~1-2GB compressed)
xgrep --build-index logs/*.gz

# Every query after that
zgrep "ERROR" logs/*.gz                # 30s
xgrep "ERROR" logs/*.gz                # 25ms
```

## Benchmarks (real production logs)

Tested on datasets from [LogHub](https://github.com/logpai/loghub). Results are cached repeated queries. [Full methodology](ARCHITECTURE.md).

| Dataset | Size (decompressed) | Query | xgrep | zgrep | Speedup |
|---|---|---|---|---|---|
| HDFS | 7.5GB | Block ID | 30ms | 27s | **913x** |
| HDFS | 7.5GB | `INFO` | 23ms | 28s | **1,217x** |
| BGL | 5.0GB | Node ID | 26ms | 17s | **655x** |
| Spark | 2.8GB | `ERROR` | 2.7s | 10m | **220x** |

### First query vs repeated query

| Mode | Time | Notes |
|---|---|---|
| `xgrep --build-index` | ~2 min | One-time cost |
| First query (no cache) | 2.4s | Parallel decompress + scan. 18x faster than zgrep. |
| Repeated query (cached) | 11-30ms | Bloom skip + mmap. |
| zgrep | 44s | Full decompress + scan. Every time. |

## Why it's fast

- **zgrep**: decompresses and scans 100% of data every time
- **xgrep**: reads 0.1-1% of data using block-level bloom filters

## Also faster than grep on plain text

While compressed-log search is xgrep's moat, the bloom prefilter pays off
on uncompressed source code too. Six head-to-head queries against GNU grep
on two real corpora — **xgrep won every one**:

| Corpus | Files | Query | xgrep | grep | Speedup |
|---|---|---|---|---|---|
| RE decompile (`.c`, mixed) | 1,164 | rare hex addr | 52ms | 253ms | **4.87x** |
| RE decompile (`.c`, mixed) | 1,164 | mid-frequency call | 63ms | 255ms | **4.05x** |
| RE decompile (`.c`, mixed) | 1,164 | universal token | 221ms | 370ms | **1.67x** |
| Python project | 25 | rare identifier | 41ms | 57ms | **1.39x** |
| Python project | 25 | `def ` | 29ms | 51ms | **1.76x** |
| Python project | 25 | `import` | 29ms | 37ms | **1.28x** |

**6/6 wins, 1.28x–4.87x range.** Indexed first, then queried — same
methodology as the gz numbers above. For maximum source-code search
throughput consider [ripgrep](https://github.com/BurntSushi/ripgrep)
(purpose-built for that workload); xgrep is the tool when compressed
archives are in the mix or you want one binary that handles both.

## Tradeoffs

- First run builds an index (cached for reuse)
- Cache stores decompressed data (~5x compressed size)
- Optimized for repeated searches over compressed logs
- For one-off plain text search: ripgrep is also a great option

## JSON mode (`-j`)

Field-aware search for NDJSON/JSONL logs:

```bash
xgrep -j 'level=error status=500' logs/*.json.gz
```

| Query | `zcat \| jq` | xgrep -j | Speedup |
|---|---|---|---|
| `user_id=42042` (9 matches) | 40.5s | 0.13s | **319x** |
| `status=503` (111K matches) | 40.5s | 1.97s | **20x** |

## All flags

```bash
xgrep "ERROR" logs/*.gz                # regex search
xgrep -F "user_id=12345" logs/*.gz     # fixed string
xgrep -i "timeout" logs/*.gz           # case insensitive
xgrep -n -C 3 "Exception" logs/*.gz    # line numbers + context
xgrep -c "WARN" logs/*.gz              # count only
xgrep -l "FATAL" logs/*.gz             # filenames only
xgrep --stats "ERROR" logs/            # show skip statistics
xgrep -j 'level=error' logs/*.jsonl.gz # JSON field search
xgrep "TODO" src/**/*.rs               # plain text (no index needed)
```

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
| `--exclude DIR` | Skip directory by name (repeatable, adds to defaults) |
| `--no-ignore` | Disable default exclusions (descend into `target`, `.git`, etc.) |

## Who this is for

Engineers who repeatedly search compressed log archives: incident investigation, log forensics, debugging. Anyone who has waited on `zgrep` and wished it were faster — without needing Elasticsearch.

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
- **Staleness**: File size + mtime checked every query. If the cache is stale, xgrep falls through to direct search on the live file rather than dropping it (v0.1.3+).
- **Default-skipped dirs**: `.git`, `target`, `node_modules`, `.venv`, `__pycache__`, `build`, `3rdparty`, `.xgrep` (self-exclude), and other common build/vendor dirs. Override with `--no-ignore` or add with `--exclude` (v0.1.4+).
- **Deep dive**: [ARCHITECTURE.md](ARCHITECTURE.md)

## License

Apache-2.0
