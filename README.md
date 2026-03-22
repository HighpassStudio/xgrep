# xgrep

Indexed search for compressed logs. 100-1,200x faster than zgrep on repeated queries.

```bash
cargo install xgrep-cli
```

```bash
xgrep --build-index logs/*.gz          # one-time: build index
xgrep "user_id=12345" logs/*.gz        # 25ms instead of 30s
```

| Dataset | Query | xgrep | zgrep | Speedup |
|---|---|---|---|---|
| HDFS (7.5GB) | Block ID | 30ms | 27s | **913x** |
| HDFS (7.5GB) | `INFO` (broad) | 23ms | 28s | **1,217x** |
| BGL (5.0GB) | Node ID | 26ms | 17s | **655x** |
| Spark (2.8GB) | `ERROR` | 2.7s | 10m | **220x** |

Benchmarked on real production logs from [LogHub](https://github.com/logpai/loghub). Cached repeated-query results. [Full benchmark methodology](ARCHITECTURE.md).

## How it works

Instead of decompressing and scanning everything on every query, xgrep:

1. **Indexes once** — decompresses `.gz` files, splits into 64KB blocks, builds a bloom filter per block
2. **Searches candidates only** — checks bloom filters (nanoseconds), memory-maps the cache, OS pages in only matching blocks (~1% of data)
3. **Outputs grep-compatibly** — same flags: `-n`, `-c`, `-l`, `-i`, `-F`, `-A/-B/-C`, color

**Key metric: bytes touched per query is ~0.1-1% of corpus vs 100% for zgrep.** The cost scales with candidate data, not corpus size.

## First query vs repeated query

| Mode | Time | Notes |
|---|---|---|
| `xgrep --build-index` | ~2 min | One-time cost |
| First query (no cache) | 2.4s | Parallel decompress + scan. 18x faster than zgrep. |
| Repeated query (cached) | 11-30ms | Bloom skip + mmap. |
| zgrep | 44s | Full decompress + scan. Every time. |

## JSON mode (`-j`)

Field-aware search for NDJSON/JSONL logs:

```bash
xgrep -j 'level=error status=500' logs/*.json.gz
```

| Query | `zcat \| jq` | xgrep -j | Speedup |
|---|---|---|---|
| `user_id=42042` (9 matches) | 40.5s | 0.13s | **319x** |
| `status=503` (111K matches) | 40.5s | 1.97s | **20x** |

## Usage

```bash
# Build index
xgrep --build-index logs/*.gz

# Search (uses cache automatically)
xgrep "ERROR" logs/*.gz
xgrep -F "user_id=12345" logs/*.gz     # fixed string
xgrep -i "timeout" logs/*.gz           # case insensitive
xgrep -n -C 3 "Exception" logs/*.gz    # line numbers + context
xgrep -c "WARN" logs/*.gz              # count only
xgrep -l "FATAL" logs/*.gz             # filenames only
xgrep --stats "ERROR" logs/            # show skip statistics

# JSON field search
xgrep -j 'level=error' logs/*.jsonl.gz

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

## Tradeoffs

- **Cache size**: ~5x compressed size (stores decompressed data + bloom index)
- **First run**: ~2 min index build for ~1-2GB compressed (amortized over all subsequent queries)
- **Not universal grep**: built for repeated search over compressed log archives
- For one-off plain text search: use ripgrep

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
- **Staleness**: File size + mtime checked every query. Stale cache ignored automatically.
- **Deep dive**: [ARCHITECTURE.md](ARCHITECTURE.md) — full architecture, bloom design, benchmark methodology

## License

Apache-2.0
