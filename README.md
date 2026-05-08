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

- **zgrep**: decompresses and scans 100% of data every time. No skip, no cache.
- **ripgrep `-z`**: decompresses fully but scans efficiently (SIMD, parallel). No persistent skip cache.
- **xgrep**: indexes once, then on repeats reads only the blocks that *might* contain the term — typically 0.1–10% of data on selective queries.

## vs ripgrep `-z` (the modern baseline)

Ripgrep with `-z` searches `.gz` files directly. xgrep adds a persistent
bloom-filter index so repeated investigation queries on the same archive
don't pay decompression cost twice. Honest head-to-head on a 56MB / 10-shard
synthetic NDJSON.gz corpus (~2.5M lines, ~250MB decompressed):

| Query | xgrep cached | ripgrep `-z` | Winner |
|---|---|---|---|
| Rare needle (1 hit) | 158ms | 320ms | **xgrep 2.0x** |
| `-j` rare field=value (1 hit) | 228ms | 314ms | **xgrep-j 1.4x** |
| Selective (~3% of lines) | 436ms | 347ms | rg 1.25x |
| Common (~70% of lines) | 4,989ms | 600ms | rg 8.3x |

**Where xgrep wins:** rare-needle queries — specific request IDs, user
IDs, error codes, trace IDs. The bloom filter skips 90%+ of blocks, and
the cached decompressed data is mmap'd for instant access on repeat runs.
This is the *investigation* pattern — searching the same archive over
and over for different specific things.

**Where ripgrep wins:** high-frequency patterns (the term is in most
blocks, so the bloom can't skip much) and one-shot searches where xgrep's
index-build cost isn't amortized. ripgrep's line scan is purpose-built
and very fast.

## When to use xgrep

```
Are you searching compressed archives?
├─ No  → use ripgrep
└─ Yes → Will you query the same archive multiple times?
        ├─ No  → use ripgrep -z (no index build needed)
        └─ Yes → Are your queries selective (rare terms)?
                ├─ No  → ripgrep -z is probably fine
                └─ Yes → use xgrep ✓
```

xgrep is an investigation tool: incident debugging, log forensics, finding
the unusual line in 50GB of gzipped logs. For source-code search, daily
grep workflows, or one-off scans, use ripgrep.

## Tradeoffs

- First run builds an index (~12s for ~50MB compressed; one-time, cached)
- Cache stores decompressed data (~5x compressed size on disk)
- Optimized for repeated *selective* searches over compressed logs
- Index is gz-corpus-shaped — small uncompressed corpora rarely justify it

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

Engineers running compressed-log investigations: incident debugging, log
forensics, on-call retros. Anyone who repeatedly searches `.gz` archives
for rare events and has waited on `zgrep` (or even `rg -z`) — without
needing Elasticsearch.

Not a general-purpose grep replacement. For source-code search, daily
greps, or one-shot scans of small files: use [ripgrep](https://github.com/BurntSushi/ripgrep).

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
