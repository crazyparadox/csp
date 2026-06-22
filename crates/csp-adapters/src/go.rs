//! Parser for Go's coverage profile (`go test -coverprofile=cover.out`).
//!
//! Format (after the `mode:` header), one block per line:
//!
//! ```text
//! mode: set
//! example.com/m/math.go:8.13,10.2 1 1
//! ```
//!
//! i.e. `file:startLine.startCol,endLine.endCol numStmts count`. Lines/cols are
//! **1-based**. A block spans a range of statements; for line coverage we mark
//! every line in `[startLine, endLine]` with that block's count, taking the max
//! when blocks overlap a line.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use csp_core::base::{Location, Range};
use csp_core::coverage::{CoverageSummary, FileCoverage, LineCoverage, RunId};
use csp_core::testing::{TestResult, TestStatus};
use serde::Deserialize;

use crate::adapters::is_ignored_dir;
use crate::path_to_uri;

/// One event from `go test -json`. Only the fields we use are deserialized.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct GoTestEvent {
    action: String,
    #[serde(default)]
    test: Option<String>,
    #[serde(default)]
    output: Option<String>,
}

/// Parse the `go test -json` event stream into [`TestResult`]s, attaching source
/// locations (scanned `func TestXxx`) and failure output messages.
pub fn parse_test_json(stdout: &str, root: &Path) -> Vec<TestResult> {
    // test name -> (status, accumulated output)
    let mut acc: BTreeMap<String, (Option<TestStatus>, String)> = BTreeMap::new();
    for line in stdout.lines() {
        let Ok(ev) = serde_json::from_str::<GoTestEvent>(line.trim()) else {
            continue;
        };
        let Some(test) = ev.test.clone() else {
            continue; // package-level event
        };
        let entry = acc.entry(test).or_insert((None, String::new()));
        match ev.action.as_str() {
            "pass" => entry.0 = Some(TestStatus::Pass),
            "fail" => entry.0 = Some(TestStatus::Fail),
            "skip" => entry.0 = Some(TestStatus::Skip),
            "output" => {
                if let Some(o) = ev.output {
                    // Drop libtest-style framing noise; keep assertion output.
                    let t = o.trim_end();
                    if !t.trim_start().starts_with("=== ")
                        && !t.trim_start().starts_with("--- ")
                        && !t.trim().is_empty()
                    {
                        entry.1.push_str(t);
                        entry.1.push('\n');
                    }
                }
            }
            _ => {}
        }
    }

    let locations = go_test_locations(root);
    acc.into_iter()
        .filter_map(|(name, (status, output))| {
            let status = status?;
            // Subtests ("TestAdd/case") share the parent func's location.
            let top = name.split('/').next().unwrap_or(&name);
            let location = locations.get(top).map(|(rel, line)| Location {
                uri: path_to_uri(root, rel),
                range: Range::whole_line(*line),
            });
            let message = (status == TestStatus::Fail && !output.trim().is_empty())
                .then(|| output.trim().to_string());
            Some(TestResult {
                id: name.clone(),
                name,
                status,
                run_id: String::new(),
                message,
                location,
                duration_ms: None,
            })
        })
        .collect()
}

/// Map each `func TestXxx`/`func BenchmarkXxx` to its source file + 0-based line.
fn go_test_locations(root: &Path) -> HashMap<String, (String, u32)> {
    fn walk(dir: &Path, root: &Path, map: &mut HashMap<String, (String, u32)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if !is_ignored_dir(&path) {
                    walk(&path, root, map);
                }
            } else if path.extension().map(|e| e == "go").unwrap_or(false) {
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                for (i, line) in content.lines().enumerate() {
                    if let Some(name) = parse_go_test_fn(line.trim_start()) {
                        map.entry(name).or_insert((rel.clone(), i as u32));
                    }
                }
            }
        }
    }
    let mut map = HashMap::new();
    walk(root, root, &mut map);
    map
}

/// Extract `Foo` from `func TestFoo(t *testing.T) {` (and Benchmark/Fuzz).
fn parse_go_test_fn(line: &str) -> Option<String> {
    let rest = line.strip_prefix("func ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (name.starts_with("Test") || name.starts_with("Benchmark") || name.starts_with("Fuzz"))
        .then_some(name)
}

/// Parse a Go coverage profile into one [`FileCoverage`] per source file.
pub fn parse(content: &str, root: &Path, run_id: &RunId) -> Vec<FileCoverage> {
    // file path -> (0-based line -> max count seen)
    let mut by_file: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("mode:") {
            continue;
        }
        let Some(block) = parse_block(line) else {
            continue;
        };
        let entry = by_file.entry(block.file).or_default();
        for l in block.start_line..=block.end_line {
            let l0 = l.saturating_sub(1);
            let slot = entry.entry(l0).or_insert(0);
            *slot = (*slot).max(block.count);
        }
    }

    by_file
        .into_iter()
        .map(|(file, lines)| {
            let lines: Vec<LineCoverage> = lines
                .into_iter()
                .map(|(line, hits)| LineCoverage { line, hits })
                .collect();
            let summary = CoverageSummary {
                lines_covered: lines.iter().filter(|l| l.hits > 0).count() as u64,
                lines_total: lines.len() as u64,
                ..Default::default()
            };
            FileCoverage {
                uri: go_path_to_uri(root, &file),
                run_id: run_id.clone(),
                version: None,
                stale: false,
                lines,
                branches: Vec::new(),
                functions: Vec::new(),
                summary,
            }
        })
        .collect()
}

struct Block {
    file: String,
    start_line: u32,
    end_line: u32,
    count: u64,
}

fn parse_block(line: &str) -> Option<Block> {
    // "<file>:<sl>.<sc>,<el>.<ec> <numStmts> <count>"
    let (file, rest) = line.rsplit_once(':')?;
    let mut fields = rest.split_whitespace();
    let span = fields.next()?;
    let _num_stmts = fields.next()?;
    let count: u64 = fields.next()?.parse().ok()?;

    let (start, end) = span.split_once(',')?;
    let start_line: u32 = start.split('.').next()?.parse().ok()?;
    let end_line: u32 = end.split('.').next()?.parse().ok()?;
    Some(Block {
        file: file.to_string(),
        start_line,
        end_line,
        count,
    })
}

/// Go profile paths are import-path-prefixed (`example.com/m/math.go`), not
/// filesystem paths. Resolve to a real file by trying the path as-is, then
/// progressively dropping leading segments until one exists under `root`, and
/// finally falling back to matching the basename.
fn go_path_to_uri(root: &Path, go_path: &str) -> String {
    if root.join(go_path).exists() {
        return path_to_uri(root, go_path);
    }
    let segments: Vec<&str> = go_path.split('/').collect();
    for i in 1..segments.len() {
        let candidate = segments[i..].join("/");
        if root.join(&candidate).exists() {
            return path_to_uri(root, &candidate);
        }
    }
    // Best effort: the raw relative path, even if it doesn't resolve on disk.
    path_to_uri(root, segments.last().copied().unwrap_or(go_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_go_test_json() {
        let stdout = concat!(
            r#"{"Action":"run","Test":"TestAdd"}"#,
            "\n",
            r#"{"Action":"pass","Test":"TestAdd"}"#,
            "\n",
            r#"{"Action":"output","Test":"TestFails","Output":"    math_test.go:10: want 5 got 9\n"}"#,
            "\n",
            r#"{"Action":"fail","Test":"TestFails"}"#,
            "\n",
            r#"{"Action":"skip","Test":"TestSkipped"}"#,
            "\n",
            r#"{"Action":"pass","Package":"p"}"#,
        );
        let results = parse_test_json(stdout, Path::new("/nonexistent"));
        assert_eq!(results.len(), 3, "package-level event excluded");
        let by = |s: TestStatus| results.iter().filter(|r| r.status == s).count();
        assert_eq!(by(TestStatus::Pass), 1);
        assert_eq!(by(TestStatus::Fail), 1);
        assert_eq!(by(TestStatus::Skip), 1);
        let fails = results.iter().find(|r| r.name == "TestFails").unwrap();
        assert!(fails.message.as_ref().unwrap().contains("want 5 got 9"));
    }

    #[test]
    fn expands_blocks_to_lines_with_max_count() {
        let profile = "\
mode: set
example.com/m/math.go:8.13,10.2 2 1
example.com/m/math.go:12.2,12.20 1 0
";
        let files = parse(profile, Path::new("/proj"), &"r".to_string());
        assert_eq!(files.len(), 1);
        let f = &files[0];
        // Lines 8,9,10 (0-based 7,8,9) covered with count 1; line 12 (0-based 11) count 0.
        let cov: Vec<_> = f.lines.iter().map(|l| (l.line, l.hits)).collect();
        assert_eq!(cov, vec![(7, 1), (8, 1), (9, 1), (11, 0)]);
        assert_eq!(f.summary.lines_covered, 3);
        assert_eq!(f.summary.lines_total, 4);
    }
}
