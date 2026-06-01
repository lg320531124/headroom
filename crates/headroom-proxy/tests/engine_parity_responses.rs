//! D2.1 — cross-language engine parity for OpenAI Responses (`/v1/responses`).
//!
//! First D2 chunk: prove a minimal Rust engine entry reproduces the
//! Python-engine golden bytes (`outbound_b64`) byte-for-byte over the existing
//! language-neutral fixtures in
//! `tests/parity/fixtures/engine_request_golden_openai/`. Those goldens were
//! recorded from the legacy handler and are byte-identical to the Python
//! `HeadroomEngine`, so reproducing them means Rust-engine == Python-engine.
//!
//! Scope: the Rust engine entry here mirrors Python's
//! `_on_request_openai_responses` MINUS memory injection (memory is OFF in the
//! golden corpus). Memory / CCR for responses come in a later chunk.

use std::fs;
use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use bytes::Bytes;
use headroom_core::auth_mode::classify as classify_auth_mode;
use headroom_proxy::compression::{compress_openai_responses_request, Outcome};
use headroom_proxy::config::CompressionMode;
use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("parity")
        .join("fixtures")
        .join("engine_request_golden_openai")
}

fn build_headers(map: &serde_json::Map<String, Value>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (key, value) in map {
        let Some(s) = value.as_str() else { continue };
        let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(s),
        ) else {
            continue;
        };
        headers.insert(name, val);
    }
    headers
}

fn is_bypass(headers: &HeaderMap) -> bool {
    let truthy = |name: &str, want: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().eq_ignore_ascii_case(want))
            .unwrap_or(false)
    };
    truthy("x-headroom-bypass", "true") || truthy("x-headroom-mode", "passthrough")
}

/// Minimal Rust engine entry for `/v1/responses`. Mirrors the Python
/// `_on_request_openai_responses` (sans memory): bypass → original bytes; else
/// mode from `optimize`; compress; `Compressed` → new body, else original.
fn engine_on_request_responses(original: Bytes, headers: &HeaderMap, optimize: bool) -> Bytes {
    if is_bypass(headers) {
        return original;
    }
    let mode = if optimize {
        CompressionMode::LiveZone
    } else {
        CompressionMode::Off
    };
    let auth_mode = classify_auth_mode(headers);
    match compress_openai_responses_request(&original, mode, auth_mode, "d2-parity-responses") {
        Outcome::Compressed { body, .. } => body,
        _ => original,
    }
}

#[test]
fn rust_engine_reproduces_python_responses_goldens() {
    let dir = fixtures_dir();
    let entries =
        fs::read_dir(&dir).unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()));

    let empty = serde_json::Map::new();
    let mut checked = 0usize;
    let mut passed = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).unwrap();
        let fix: Value = serde_json::from_str(&raw).unwrap();

        if fix.get("endpoint").and_then(Value::as_str) != Some("/v1/responses") {
            continue;
        }
        if fix.get("nondeterministic_flag").and_then(Value::as_bool) == Some(true) {
            continue;
        }

        let name = fix
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let inbound = STANDARD
            .decode(fix.get("inbound_b64").and_then(Value::as_str).unwrap())
            .unwrap();
        let expected = STANDARD
            .decode(fix.get("outbound_b64").and_then(Value::as_str).unwrap())
            .unwrap();
        let headers = build_headers(fix.get("headers").and_then(Value::as_object).unwrap_or(&empty));
        let optimize = fix
            .get("proxy_config")
            .and_then(|c| c.get("optimize"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let got = engine_on_request_responses(Bytes::from(inbound), &headers, optimize);
        checked += 1;
        if got.as_ref() == expected.as_slice() {
            passed += 1;
        } else {
            failures.push(format!(
                "  {name}: engine={} bytes, golden={} bytes\n    engine_prefix={}\n    golden_prefix={}",
                got.len(),
                expected.len(),
                String::from_utf8_lossy(&got[..got.len().min(200)]),
                String::from_utf8_lossy(&expected[..expected.len().min(200)]),
            ));
        }
    }

    assert!(
        checked >= 6,
        "expected >=6 /v1/responses fixtures, checked {checked}"
    );
    eprintln!("D2.1 responses parity: {passed}/{checked} byte-exact");
    assert!(
        failures.is_empty(),
        "{}/{} responses fixtures diverged:\n{}",
        failures.len(),
        checked,
        failures.join("\n")
    );
}
