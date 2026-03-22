/// JSON field filter parsing for NDJSON/JSONL search.
///
/// Supported query syntax:
///   field=value              — top-level field equality
///   field.subfield=value     — one-level nested (dot notation)
///   field="quoted value"     — quoted values (spaces allowed)
///   f1=v1 f2=v2             — AND logic (all must match)
///
/// Scope: line-delimited JSON logs, equality filters, AND semantics.
/// Not supported: arrays, OR, range queries, deep paths, jq expressions.

use serde_json::Value;

/// A single field=value filter clause.
#[derive(Debug, Clone)]
pub struct JsonFilter {
    /// Field path: "status" or "http.method"
    pub field: String,
    /// Expected value (lowercase for case-insensitive matching)
    pub value: String,
}

/// Parse a JSON filter query string into filter clauses.
///
/// Input: "user_id=12345 status=500 http.method=POST"
/// Output: vec of JsonFilter
pub fn parse_json_query(query: &str) -> Result<Vec<JsonFilter>, String> {
    let mut filters = Vec::new();
    let mut chars = query.chars().peekable();

    // Skip leading whitespace
    skip_ws(&mut chars);

    while chars.peek().is_some() {
        // Read field name (everything up to '=')
        let mut field = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' {
                break;
            }
            if c.is_whitespace() {
                // Whitespace before '=' means malformed — but be tolerant
                break;
            }
            field.push(c);
            chars.next();
        }

        if field.is_empty() {
            skip_ws(&mut chars);
            continue;
        }

        // Expect '='
        match chars.peek() {
            Some(&'=') => {
                chars.next();
            }
            _ => {
                return Err(format!("expected '=' after field '{}'", field));
            }
        }

        // Read value — either quoted or unquoted
        let value = match chars.peek() {
            Some(&'"') => {
                chars.next(); // consume opening quote
                let mut v = String::new();
                let mut escaped = false;
                loop {
                    match chars.next() {
                        None => return Err("unterminated quoted value".to_string()),
                        Some('\\') if !escaped => {
                            escaped = true;
                        }
                        Some('"') if !escaped => break,
                        Some(c) => {
                            if escaped {
                                v.push('\\');
                                escaped = false;
                            }
                            v.push(c);
                        }
                    }
                }
                v
            }
            _ => {
                let mut v = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        break;
                    }
                    v.push(c);
                    chars.next();
                }
                v
            }
        };

        if field.is_empty() {
            return Err("empty field name".to_string());
        }

        filters.push(JsonFilter {
            field: field.to_ascii_lowercase(),
            value: value.to_ascii_lowercase(),
        });

        skip_ws(&mut chars);
    }

    if filters.is_empty() {
        return Err("no filter clauses found".to_string());
    }

    Ok(filters)
}

fn skip_ws(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while let Some(&c) = chars.peek() {
        if !c.is_whitespace() {
            break;
        }
        chars.next();
    }
}

/// Check if a JSON line matches all filter clauses.
/// Returns true if ALL filters match (AND logic).
/// Uses loose equality: numeric 500 matches string "500".
pub fn line_matches_filters(line: &str, filters: &[JsonFilter]) -> bool {
    let val: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return false, // not valid JSON — skip
    };

    let obj = match val.as_object() {
        Some(o) => o,
        None => return false, // not a JSON object — skip
    };

    for filter in filters {
        if !field_matches(obj, &filter.field, &filter.value) {
            return false;
        }
    }
    true
}

/// Check if a single field=value filter matches against a JSON object.
/// Supports dot notation for one level of nesting.
fn field_matches(
    obj: &serde_json::Map<String, Value>,
    field_path: &str,
    expected: &str,
) -> bool {
    if let Some(dot_pos) = field_path.find('.') {
        // Nested: "http.status" → obj["http"]["status"]
        let parent = &field_path[..dot_pos];
        let child = &field_path[dot_pos + 1..];

        // Case-insensitive field lookup
        for (key, val) in obj {
            if key.to_ascii_lowercase() == parent {
                if let Some(inner) = val.as_object() {
                    for (ikey, ival) in inner {
                        if ikey.to_ascii_lowercase() == child {
                            return value_equals(ival, expected);
                        }
                    }
                }
                return false;
            }
        }
        false
    } else {
        // Top-level
        for (key, val) in obj {
            if key.to_ascii_lowercase() == field_path {
                return value_equals(val, expected);
            }
        }
        false
    }
}

/// Loose equality: compare JSON value against expected string.
/// - String "500" matches expected "500"
/// - Number 500 matches expected "500"
/// - Boolean true matches expected "true"
/// - Null matches expected "null"
/// All comparisons are case-insensitive.
fn value_equals(val: &Value, expected: &str) -> bool {
    let actual = value_to_string(val);
    actual == expected
}

/// Stringify a JSON value for comparison and bloom insertion.
/// Numbers, bools, nulls → canonical string form.
/// Strings → as-is (lowercased by caller).
/// Objects/arrays → skip (return empty).
pub fn value_to_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.to_ascii_lowercase(),
        Value::Number(n) => {
            // Prefer integer form if possible
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(f) = n.as_f64() {
                // Avoid trailing .0 for whole numbers
                if f.fract() == 0.0 && f.abs() < i64::MAX as f64 {
                    (f as i64).to_string()
                } else {
                    f.to_string()
                }
            } else {
                n.to_string()
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(_) | Value::Object(_) => String::new(), // skip complex values
    }
}

/// Extract field-value pairs from a JSON line for bloom insertion.
/// Returns (field_name_lowercase, value_string_lowercase) pairs.
/// Handles top-level fields and one level of nesting (dot notation).
/// Skips values > 128 bytes and limits to 64 fields per line.
pub fn extract_json_fields(line: &str) -> Vec<(String, String)> {
    let val: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let obj = match val.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };

    const MAX_FIELDS: usize = 64;
    const MAX_VALUE_LEN: usize = 128;

    let mut pairs = Vec::new();

    for (key, val) in obj {
        if pairs.len() >= MAX_FIELDS {
            break;
        }

        let field = key.to_ascii_lowercase();

        match val {
            Value::Object(inner) => {
                // One level of nesting
                for (ikey, ival) in inner {
                    if pairs.len() >= MAX_FIELDS {
                        break;
                    }
                    let vs = value_to_string(ival);
                    if vs.is_empty() || vs.len() > MAX_VALUE_LEN {
                        continue;
                    }
                    let nested_field = format!("{}.{}", field, ikey.to_ascii_lowercase());
                    pairs.push((nested_field, vs));
                }
            }
            Value::Array(_) => continue, // skip arrays
            _ => {
                let vs = value_to_string(val);
                if vs.is_empty() || vs.len() > MAX_VALUE_LEN {
                    continue;
                }
                pairs.push((field, vs));
            }
        }
    }

    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let filters = parse_json_query("user_id=12345").unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "user_id");
        assert_eq!(filters[0].value, "12345");
    }

    #[test]
    fn test_parse_multiple() {
        let filters = parse_json_query("level=error status=500").unwrap();
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0].field, "level");
        assert_eq!(filters[0].value, "error");
        assert_eq!(filters[1].field, "status");
        assert_eq!(filters[1].value, "500");
    }

    #[test]
    fn test_parse_quoted() {
        let filters = parse_json_query("message=\"timeout error\"").unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "message");
        assert_eq!(filters[0].value, "timeout error");
    }

    #[test]
    fn test_parse_dot_path() {
        let filters = parse_json_query("http.method=post").unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "http.method");
        assert_eq!(filters[0].value, "post");
    }

    #[test]
    fn test_parse_case_insensitive() {
        let filters = parse_json_query("Level=ERROR").unwrap();
        assert_eq!(filters[0].field, "level");
        assert_eq!(filters[0].value, "error");
    }

    #[test]
    fn test_parse_empty_rejected() {
        assert!(parse_json_query("").is_err());
        assert!(parse_json_query("   ").is_err());
    }

    #[test]
    fn test_line_matches_string() {
        let filters = parse_json_query("level=error").unwrap();
        assert!(line_matches_filters(r#"{"level":"ERROR","msg":"fail"}"#, &filters));
        assert!(!line_matches_filters(r#"{"level":"INFO","msg":"ok"}"#, &filters));
    }

    #[test]
    fn test_line_matches_number_loose() {
        let filters = parse_json_query("status=500").unwrap();
        // Numeric 500
        assert!(line_matches_filters(r#"{"status":500,"msg":"fail"}"#, &filters));
        // String "500"
        assert!(line_matches_filters(r#"{"status":"500","msg":"fail"}"#, &filters));
    }

    #[test]
    fn test_line_matches_nested() {
        let filters = parse_json_query("http.method=post").unwrap();
        assert!(line_matches_filters(
            r#"{"http":{"method":"POST","path":"/api"}}"#,
            &filters
        ));
    }

    #[test]
    fn test_line_matches_and_logic() {
        let filters = parse_json_query("level=error status=500").unwrap();
        assert!(line_matches_filters(
            r#"{"level":"ERROR","status":500}"#,
            &filters
        ));
        assert!(!line_matches_filters(
            r#"{"level":"ERROR","status":200}"#,
            &filters
        ));
    }

    #[test]
    fn test_line_matches_not_json() {
        let filters = parse_json_query("level=error").unwrap();
        assert!(!line_matches_filters("not json at all", &filters));
    }

    #[test]
    fn test_extract_json_fields() {
        let pairs = extract_json_fields(r#"{"level":"ERROR","status":500,"http":{"method":"POST"}}"#);
        assert!(pairs.contains(&("level".to_string(), "error".to_string())));
        assert!(pairs.contains(&("status".to_string(), "500".to_string())));
        assert!(pairs.contains(&("http.method".to_string(), "post".to_string())));
    }

    #[test]
    fn test_extract_skips_long_values() {
        let long_val = "x".repeat(200);
        let json = format!(r#"{{"msg":"{}","level":"info"}}"#, long_val);
        let pairs = extract_json_fields(&json);
        // msg should be skipped (>128), level should be present
        assert!(!pairs.iter().any(|(f, _)| f == "msg"));
        assert!(pairs.iter().any(|(f, _)| f == "level"));
    }

    #[test]
    fn test_extract_skips_arrays() {
        let pairs = extract_json_fields(r#"{"tags":["a","b"],"level":"info"}"#);
        assert!(!pairs.iter().any(|(f, _)| f == "tags"));
        assert!(pairs.iter().any(|(f, _)| f == "level"));
    }
}
