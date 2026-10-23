use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::extract::extract;
use super::model::{Lang, SymbolKind};

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Expected {
    symbols: Vec<ExpectedSymbol>,
    calls: Vec<ExpectedCall>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ExpectedSymbol {
    qname: String,
    kind: SymbolKind,
    start: u32,
    end: u32,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ExpectedCall {
    caller: String,
    callee: String,
    line: u32,
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code")
}

#[test]
fn code_fixtures_match() {
    let fixtures = [
        ("rust/features.rs", Lang::Rust),
        ("typescript/features.ts", Lang::TypeScript),
        ("javascript/features.js", Lang::JavaScript),
        ("python/features.py", Lang::Python),
        ("go/features.go", Lang::Go),
        ("java/Features.java", Lang::Java),
        ("kotlin/Features.kt", Lang::Kotlin),
        ("csharp/Features.cs", Lang::CSharp),
    ];
    for (relative, lang) in fixtures {
        let path = fixtures_root().join(relative);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read fixture {}: {error}", path.display()));
        let expected_path = path.with_file_name(format!(
            "{}.expected.json",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        ));
        let expected_source = fs::read_to_string(&expected_path).unwrap_or_else(|error| {
            panic!(
                "cannot read expected file {}: {error}",
                expected_path.display()
            )
        });
        let expected: Expected = serde_json::from_str(&expected_source).unwrap_or_else(|error| {
            panic!("invalid expected file {}: {error}", expected_path.display())
        });
        let outline = extract(lang, &source);
        let actual = Expected {
            symbols: outline
                .symbols
                .into_iter()
                .map(|symbol| ExpectedSymbol {
                    qname: symbol.qname,
                    kind: symbol.kind,
                    start: symbol.start_line,
                    end: symbol.end_line,
                })
                .collect(),
            calls: outline
                .calls
                .into_iter()
                .map(|call| ExpectedCall {
                    caller: call.caller_qname,
                    callee: call.callee,
                    line: call.line,
                })
                .collect(),
        };
        assert_eq!(actual, expected, "fixture {}", path.display());
    }
}

#[test]
fn extract_is_total_on_truncated_and_generated_inputs() {
    let fragments = [
        "\"unterminated",
        "/* nested /* comment",
        "fn broken<T(",
        "class C { void M(",
        "def broken(\n    f'''text",
        "`template ${ { nested: true }",
        "@\"verbatim \"\" text",
        "\0\u{7f}\u{80}{}()[]",
    ];
    for lang in [
        Lang::Rust,
        Lang::TypeScript,
        Lang::JavaScript,
        Lang::Python,
        Lang::Go,
        Lang::Java,
        Lang::Kotlin,
        Lang::CSharp,
    ] {
        for fragment in fragments {
            let outcome = std::panic::catch_unwind(|| extract(lang, fragment));
            assert!(outcome.is_ok(), "{lang:?} panicked on {fragment:?}");
        }
        let mut state = 0x9e37_79b9_u32;
        for length in 0..256usize {
            let generated: String = (0..length)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    const ALPHABET: &[u8] = b"abcXYZ_012{}()[]/'\"`#$:;,.<>!= \\n";
                    ALPHABET[state as usize % ALPHABET.len()] as char
                })
                .collect();
            let outcome = std::panic::catch_unwind(|| extract(lang, &generated));
            assert!(outcome.is_ok(), "{lang:?} panicked on generated input");
        }
    }
}

#[test]
#[ignore = "release-only performance budget"]
fn extracts_one_megabyte_under_fifty_milliseconds_in_release() {
    let unit = "pub fn generated(value: usize) -> usize { helper(value) }\n";
    let source = unit.repeat(1_048_576usize.div_ceil(unit.len()));
    let source = &source[..1_048_576];
    let started = std::time::Instant::now();
    let _ = extract(Lang::Rust, source);
    let elapsed = started.elapsed();
    if !cfg!(debug_assertions) {
        assert!(elapsed.as_millis() < 50, "extraction took {elapsed:?}");
    }
}
