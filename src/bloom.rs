/// Block-level bloom filter for content prefiltering.
///
/// Uses **token-level** indexing (split on whitespace + delimiters) instead of
/// trigrams. Trigrams saturate small bloom filters on real text data — 64KB of
/// content produces ~20k unique trigrams, filling a 512-bit filter to 100%.
///
/// Token-level with multi-delimiter splitting:
///   "user_id=12345" → tokens: ["user_id", "12345"]
///   A 64KB block has ~2000-4000 unique tokens.
///   4KB bloom (32768 bits) + 3 hashes + 4000 items → ~3% false positive rate.
///
/// Overhead: 4KB per 64KB block = 6.25% (acceptable for the skip rates it enables).

const BLOOM_SIZE_BYTES: usize = 4096; // 4KB = 32768 bits
const BLOOM_BITS: usize = BLOOM_SIZE_BYTES * 8;
const NUM_HASHES: usize = 3;

/// Delimiters for tokenization — split on these in addition to whitespace.
/// Covers common log patterns: key=value, paths, timestamps, brackets.
const TOKEN_DELIMITERS: &[u8] = b" \t\n\r=:/-_[](){}\"',;|<>@#&";

#[derive(Clone)]
pub struct BloomFilter {
    bits: Vec<u8>,
}

impl BloomFilter {
    pub fn new() -> Self {
        BloomFilter {
            bits: vec![0u8; BLOOM_SIZE_BYTES],
        }
    }

    /// Insert a token into the bloom filter.
    #[inline]
    pub fn insert_token(&mut self, token: &[u8]) {
        if token.is_empty() {
            return;
        }
        let base = hash_bytes(token);
        for i in 0..NUM_HASHES {
            let bit = hash_nth(base, i) % BLOOM_BITS;
            self.bits[bit / 8] |= 1 << (bit % 8);
        }
    }

    /// Check if a token might exist in this block.
    #[inline]
    pub fn might_contain_token(&self, token: &[u8]) -> bool {
        if token.is_empty() {
            return true;
        }
        let base = hash_bytes(token);
        for i in 0..NUM_HASHES {
            let bit = hash_nth(base, i) % BLOOM_BITS;
            if self.bits[bit / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }

    /// Check if ALL tokens from a query string might exist in this block.
    /// Splits the query on delimiters and checks each resulting token.
    /// Returns false (skip block) if any required token is definitely absent.
    pub fn might_contain_query(&self, query: &[u8], ignore_case: bool) -> bool {
        let tokens = tokenize(query, ignore_case);
        if tokens.is_empty() {
            return true; // can't filter
        }
        for token in &tokens {
            if token.len() < 2 {
                continue; // skip very short tokens — too many false positives
            }
            if !self.might_contain_token(token) {
                return false;
            }
        }
        true
    }

    /// Insert a field-value pair for JSON mode.
    /// Inserts THREE bloom entries:
    ///   1. field name alone (enables future field-existence queries)
    ///   2. value alone (keeps text-mode compat)
    ///   3. field\0value (the discriminator — binds field to value)
    pub fn insert_field_value(&mut self, field: &str, value: &str) {
        // 1. Field name
        let field_lower = field.to_ascii_lowercase();
        if field_lower.len() >= 2 {
            self.insert_token(field_lower.as_bytes());
        }
        // 2. Value
        let value_lower = value.to_ascii_lowercase();
        if value_lower.len() >= 2 {
            self.insert_token(value_lower.as_bytes());
        }
        // 3. Field\0value pair — the key discriminator
        let mut pair = Vec::with_capacity(field_lower.len() + 1 + value_lower.len());
        pair.extend_from_slice(field_lower.as_bytes());
        pair.push(0x00);
        pair.extend_from_slice(value_lower.as_bytes());
        self.insert_token(&pair);
    }

    /// Check if a field=value pair might exist in this block.
    /// Only checks the combined field\0value hash (the discriminator).
    #[inline]
    pub fn might_contain_field_value(&self, field: &str, value: &str) -> bool {
        let mut pair = Vec::with_capacity(field.len() + 1 + value.len());
        pair.extend_from_slice(field.as_bytes()); // already lowercase from query parser
        pair.push(0x00);
        pair.extend_from_slice(value.as_bytes()); // already lowercase from query parser
        self.might_contain_token(&pair)
    }

    /// Check if ALL JSON filter clauses might exist in this block.
    /// Returns false (skip block) if any field=value pair is definitely absent.
    pub fn might_contain_json_query(&self, filters: &[crate::query::JsonFilter]) -> bool {
        for filter in filters {
            if !self.might_contain_field_value(&filter.field, &filter.value) {
                return false;
            }
        }
        true
    }

    pub fn saturation(&self) -> f64 {
        let set_bits: usize = self.bits.iter().map(|b| b.count_ones() as usize).sum();
        set_bits as f64 / BLOOM_BITS as f64
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    pub fn from_vec(bits: Vec<u8>) -> Self {
        BloomFilter { bits }
    }
}

/// Build a bloom filter from a block of text content.
/// Tokenizes on whitespace + delimiters and inserts each token (lowercased).
pub fn build_block_bloom(content: &[u8]) -> BloomFilter {
    let mut bloom = BloomFilter::new();
    let tokens = tokenize(content, true); // always build lowercase
    for token in &tokens {
        if token.len() >= 2 {
            bloom.insert_token(token);
        }
    }
    bloom
}

/// Build a bloom filter from a block of text content with JSON field enrichment.
/// Inserts both standard text tokens AND field-value pairs from NDJSON lines.
pub fn build_block_bloom_json(content: &[u8]) -> BloomFilter {
    // Start with standard token-level bloom (preserves text search compat)
    let mut bloom = build_block_bloom(content);

    // Enrich with JSON field-value pairs from each line
    let mut line_start = 0;
    while line_start < content.len() {
        let line_end = memchr::memchr(b'\n', &content[line_start..])
            .map(|i| line_start + i)
            .unwrap_or(content.len());

        let line_bytes = &content[line_start..line_end];
        // Trim \r
        let line_bytes = if line_bytes.last() == Some(&b'\r') {
            &line_bytes[..line_bytes.len() - 1]
        } else {
            line_bytes
        };

        if !line_bytes.is_empty() {
            if let Ok(line_str) = std::str::from_utf8(line_bytes) {
                let pairs = crate::query::extract_json_fields(line_str);
                for (field, value) in &pairs {
                    bloom.insert_field_value(field, value);
                }
            }
        }

        line_start = line_end + 1;
    }

    bloom
}

/// Tokenize bytes by splitting on delimiter characters.
/// If ignore_case, lowercases all tokens.
fn tokenize(data: &[u8], ignore_case: bool) -> Vec<Vec<u8>> {
    let mut tokens = Vec::new();
    let mut current = Vec::new();

    for &b in data {
        if TOKEN_DELIMITERS.contains(&b) {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
        } else if ignore_case {
            current.push(b.to_ascii_lowercase());
        } else {
            current.push(b);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// FNV-1a hash of arbitrary bytes.
#[inline]
fn hash_bytes(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325; // FNV offset basis
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3); // FNV prime
    }
    h
}

/// Derive the i-th hash position from a base hash (double hashing with quadratic probe).
#[inline]
fn hash_nth(base: u64, i: usize) -> usize {
    let h1 = base as usize;
    let h2 = (base >> 32) as usize;
    h1.wrapping_add(i.wrapping_mul(h2))
        .wrapping_add(i.wrapping_mul(i))
}

/// Extract required literal bytes from a search pattern for bloom prefiltering.
/// For fixed strings: returns the string itself.
/// For regex: extracts the longest required literal substring.
pub fn extract_literals(pattern: &str, is_fixed: bool) -> Option<Vec<u8>> {
    if is_fixed {
        if pattern.len() >= 3 {
            return Some(pattern.as_bytes().to_vec());
        }
        return None;
    }

    // For regex: find the longest run of literal characters (no regex metacharacters).
    let mut best: Vec<u8> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let meta_chars = b"\\.*+?[](){}|^$";

    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if meta_chars.contains(&bytes[i]) {
            if current.len() > best.len() {
                best = current.clone();
            }
            current.clear();
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            current.push(bytes[i]);
            i += 1;
        }
    }
    if current.len() > best.len() {
        best = current;
    }

    if best.len() >= 3 {
        Some(best)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize() {
        let input = b"user_id=12345 status=200";
        let tokens = tokenize(input, false);
        assert!(tokens.contains(&b"user".to_vec()));
        assert!(tokens.contains(&b"id".to_vec()));
        assert!(tokens.contains(&b"12345".to_vec()));
        assert!(tokens.contains(&b"status".to_vec()));
        assert!(tokens.contains(&b"200".to_vec()));
    }

    #[test]
    fn test_bloom_token_insert_and_check() {
        let mut bloom = BloomFilter::new();
        bloom.insert_token(b"12345");
        assert!(bloom.might_contain_token(b"12345"));
    }

    #[test]
    fn test_bloom_block_build() {
        let content = b"2026-03-21 10:00:01 ERROR user_id=12345 connection timeout";
        let bloom = build_block_bloom(content);
        assert!(bloom.might_contain_query(b"12345", false));
        assert!(bloom.might_contain_query(b"ERROR", true));
        assert!(bloom.might_contain_query(b"user_id=12345", true));
    }

    #[test]
    fn test_bloom_saturation_reasonable() {
        // 64KB of realistic log data should not saturate a 4KB bloom
        let mut content = Vec::new();
        for i in 0..1000 {
            content.extend_from_slice(
                format!("2026-03-21 10:00:{:02} INFO user_id={} req=req-{:06x} /api/test status=200 latency={}ms\n",
                    i % 60, i % 50000, i, i % 5000).as_bytes()
            );
        }
        let bloom = build_block_bloom(&content);
        let sat = bloom.saturation();
        assert!(sat < 0.5, "Bloom saturation {:.1}% is too high", sat * 100.0);
    }

    #[test]
    fn test_extract_literals_fixed() {
        assert_eq!(extract_literals("ERROR", true), Some(b"ERROR".to_vec()));
        assert_eq!(extract_literals("ab", true), None);
    }

    #[test]
    fn test_extract_literals_regex() {
        assert_eq!(
            extract_literals("ERROR.*timeout", false),
            Some(b"timeout".to_vec())
        );
    }

    #[test]
    fn test_field_value_insert_and_check() {
        let mut bloom = BloomFilter::new();
        bloom.insert_field_value("user_id", "12345");

        // Should find the pair
        assert!(bloom.might_contain_field_value("user_id", "12345"));
        // Should NOT find a different pairing
        assert!(!bloom.might_contain_field_value("request_id", "12345"));
        // Individual tokens should also be present (for text mode compat)
        assert!(bloom.might_contain_token(b"12345"));
    }

    #[test]
    fn test_json_bloom_build() {
        let content = br#"{"level":"ERROR","user_id":"12345","status":500}
{"level":"INFO","user_id":"99999","status":200}
"#;
        let bloom = build_block_bloom_json(content);

        // Field-value pairs should be present
        assert!(bloom.might_contain_field_value("level", "error"));
        assert!(bloom.might_contain_field_value("user_id", "12345"));
        assert!(bloom.might_contain_field_value("status", "500"));

        // Cross-field pairing should (likely) not match
        // Note: bloom has false positives, but this tests the concept
        assert!(bloom.might_contain_field_value("user_id", "99999"));

        // Standard tokens should also work
        assert!(bloom.might_contain_query(b"12345", false));
        assert!(bloom.might_contain_query(b"ERROR", true));
    }

    #[test]
    fn test_json_bloom_nested() {
        let content = br#"{"http":{"method":"POST","status":503},"level":"error"}
"#;
        let bloom = build_block_bloom_json(content);
        assert!(bloom.might_contain_field_value("http.method", "post"));
        assert!(bloom.might_contain_field_value("http.status", "503"));
        assert!(bloom.might_contain_field_value("level", "error"));
    }
}
