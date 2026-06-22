//! Parser for Istanbul's `coverage-final.json` (the format Jest, Vitest, c8 and
//! nyc all emit). Each top-level key is an absolute file path mapping to maps of
//! statements/functions/branches and their hit counts:
//!
//! ```jsonc
//! {
//!   "/abs/file.ts": {
//!     "path": "/abs/file.ts",
//!     "statementMap": { "0": { "start": {"line":1,"column":0}, "end": {...} } },
//!     "s": { "0": 3 },
//!     "fnMap": { "0": { "name": "add", "decl": { "start": {...}, "end": {...} } } },
//!     "f": { "0": 2 },
//!     "branchMap": { "0": { "loc": {...}, "locations": [ {...}, {...} ] } },
//!     "b": { "0": [3, 0] }
//!   }
//! }
//! ```
//!
//! Istanbul lines are **1-based**. We project statements onto line coverage
//! (max hit count per line), and carry functions and branches through directly.

use std::collections::BTreeMap;
use std::path::Path;

use csp_core::base::{Position, Range};
use csp_core::base::Location;
use csp_core::coverage::{
    BranchCoverage, CoverageSummary, FileCoverage, FunctionCoverage, LineCoverage, RunId,
};
use csp_core::testing::{TestResult, TestStatus};
use serde_json::Value;

use crate::path_to_uri;

/// Parse Vitest's/Jest's JSON test report into [`TestResult`]s. Each file's
/// `assertionResults` become tests; locations come from the report when present,
/// else from scanning the file for `test("title")` / `it("title")`.
pub fn parse_vitest_json(json: &str, root: &Path) -> Vec<TestResult> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(files) = v.get("testResults").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for file in files {
        let abs = file.get("name").and_then(Value::as_str).unwrap_or("");
        let rel = relative_to(root, abs);
        for a in file
            .get("assertionResults")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let title = a.get("title").and_then(Value::as_str).unwrap_or("").to_string();
            let ancestors: Vec<&str> = a
                .get("ancestorTitles")
                .and_then(Value::as_array)
                .map(|x| x.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let name = if ancestors.is_empty() {
                title.clone()
            } else {
                format!("{} › {}", ancestors.join(" › "), title)
            };
            let status = match a.get("status").and_then(Value::as_str) {
                Some("passed") => TestStatus::Pass,
                Some("failed") => TestStatus::Fail,
                _ => TestStatus::Skip, // pending / skipped / todo
            };
            let message = if status == TestStatus::Fail {
                a.get("failureMessages")
                    .and_then(Value::as_array)
                    .map(|m| {
                        m.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .filter(|s| !s.trim().is_empty())
            } else {
                None
            };
            // Location: from the report if present, else scan the file for the title.
            let line = a
                .get("location")
                .and_then(|l| l.get("line"))
                .and_then(Value::as_u64)
                .map(|n| (n as u32).saturating_sub(1))
                .or_else(|| rel.as_deref().and_then(|r| scan_test_line(root, r, &title)));
            let location = match (&rel, line) {
                (Some(r), Some(l)) => Some(Location {
                    uri: path_to_uri(root, r),
                    range: csp_core::base::Range::whole_line(l),
                }),
                _ => None,
            };
            out.push(TestResult {
                id: name.clone(),
                name,
                status,
                run_id: String::new(),
                message,
                location,
                duration_ms: None,
            });
        }
    }
    out
}

/// Strip `root` from an absolute path → a root-relative path (or `None`).
fn relative_to(root: &Path, abs: &str) -> Option<String> {
    Path::new(abs)
        .strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Find the 0-based line of `test("title")` / `it("title")` in a source file.
fn scan_test_line(root: &Path, rel: &str, title: &str) -> Option<u32> {
    let content = std::fs::read_to_string(root.join(rel)).ok()?;
    let needles = [
        format!("\"{title}\""),
        format!("'{title}'"),
        format!("`{title}`"),
    ];
    content.lines().enumerate().find_map(|(i, line)| {
        let t = line.trim_start();
        let is_test = t.starts_with("test(")
            || t.starts_with("it(")
            || t.starts_with("test.")
            || t.starts_with("it.");
        (is_test && needles.iter().any(|n| line.contains(n.as_str()))).then_some(i as u32)
    })
}

/// Parse an Istanbul coverage document. Returns an error if the JSON is invalid.
pub fn parse(content: &str, root: &Path, run_id: &RunId) -> anyhow::Result<Vec<FileCoverage>> {
    let root_val: Value = serde_json::from_str(content)?;
    let obj = root_val
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("istanbul coverage must be a JSON object"))?;

    let mut out = Vec::new();
    for (path, file_val) in obj {
        out.push(parse_file(path, file_val, root, run_id));
    }
    Ok(out)
}

fn parse_file(path: &str, v: &Value, root: &Path, run_id: &RunId) -> FileCoverage {
    let file_path = v.get("path").and_then(Value::as_str).unwrap_or(path);

    // statementMap[id].start.line + s[id] -> per-line max hits.
    let mut line_hits: BTreeMap<u32, u64> = BTreeMap::new();
    if let (Some(stmt_map), Some(s)) = (obj(v, "statementMap"), obj(v, "s")) {
        for (id, loc) in stmt_map {
            let hits = s.get(id).and_then(Value::as_u64).unwrap_or(0);
            if let Some(line) = loc.get("start").and_then(|p| line0(p)) {
                let slot = line_hits.entry(line).or_insert(0);
                *slot = (*slot).max(hits);
            }
        }
    }
    let lines: Vec<LineCoverage> = line_hits
        .into_iter()
        .map(|(line, hits)| LineCoverage { line, hits })
        .collect();

    // fnMap[id].decl + f[id] -> functions.
    let mut functions = Vec::new();
    if let (Some(fn_map), Some(f)) = (obj(v, "fnMap"), obj(v, "f")) {
        for (id, fnv) in fn_map {
            let hits = f.get(id).and_then(Value::as_u64).unwrap_or(0);
            let name = fnv
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<anonymous>")
                .to_string();
            let range = fnv
                .get("decl")
                .or_else(|| fnv.get("loc"))
                .and_then(range_of)
                .unwrap_or_default();
            functions.push(FunctionCoverage { name, range, hits });
        }
    }

    // branchMap[id].loc + b[id] (array of arm hit counts) -> branches.
    let mut branches = Vec::new();
    if let (Some(branch_map), Some(b)) = (obj(v, "branchMap"), obj(v, "b")) {
        for (id, bv) in branch_map {
            let range = bv
                .get("loc")
                .and_then(range_of)
                .unwrap_or_default();
            let arms: Vec<u64> = b
                .get(id)
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|x| x.as_u64().unwrap_or(0)).collect())
                .unwrap_or_default();
            branches.push(BranchCoverage { range, arms });
        }
    }

    let summary = CoverageSummary {
        lines_covered: lines.iter().filter(|l| l.hits > 0).count() as u64,
        lines_total: lines.len() as u64,
        branches_covered: branches.iter().flat_map(|b| &b.arms).filter(|&&h| h > 0).count() as u64,
        branches_total: branches.iter().map(|b| b.arms.len() as u64).sum(),
        functions_covered: functions.iter().filter(|f| f.hits > 0).count() as u64,
        functions_total: functions.len() as u64,
    };

    FileCoverage {
        uri: path_to_uri(root, file_path),
        run_id: run_id.clone(),
        version: None,
        stale: false,
        lines,
        branches,
        functions,
        summary,
    }
}

fn obj<'a>(v: &'a Value, key: &str) -> Option<&'a serde_json::Map<String, Value>> {
    v.get(key).and_then(Value::as_object)
}

/// Istanbul position -> 0-based line.
fn line0(p: &Value) -> Option<u32> {
    let line = p.get("line").and_then(Value::as_u64)? as u32;
    Some(line.saturating_sub(1))
}

/// Build a 0-based [`Range`] from an Istanbul `{ start, end }` location.
fn range_of(loc: &Value) -> Option<Range> {
    let start = pos_of(loc.get("start")?)?;
    let end = loc.get("end").and_then(pos_of).unwrap_or(start);
    Some(Range::new(start, end))
}

fn pos_of(p: &Value) -> Option<Position> {
    let line = (p.get("line")?.as_u64()? as u32).saturating_sub(1);
    let character = p.get("column").and_then(Value::as_u64).unwrap_or(0) as u32;
    Some(Position::new(line, character))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vitest_json() {
        let json = r#"{ "testResults": [ {
            "name": "/proj/src/math.test.ts",
            "assertionResults": [
              { "ancestorTitles": [], "title": "add", "status": "passed", "location": { "line": 4, "column": 1 } },
              { "ancestorTitles": ["math"], "title": "sub", "status": "failed", "failureMessages": ["expected 1 to be 2"] }
            ]
        } ] }"#;
        let results = parse_vitest_json(json, Path::new("/proj"));
        assert_eq!(results.len(), 2);
        let add = results.iter().find(|r| r.name == "add").unwrap();
        assert_eq!(add.status, TestStatus::Pass);
        assert_eq!(add.location.as_ref().unwrap().range.start.line, 3); // 1-based 4 → 0-based 3
        let sub = results.iter().find(|r| r.name == "math › sub").unwrap();
        assert_eq!(sub.status, TestStatus::Fail);
        assert!(sub.message.as_ref().unwrap().contains("expected 1 to be 2"));
    }

    const SAMPLE: &str = r#"{
      "/proj/src/math.ts": {
        "path": "/proj/src/math.ts",
        "statementMap": {
          "0": { "start": { "line": 1, "column": 0 }, "end": { "line": 1, "column": 10 } },
          "1": { "start": { "line": 2, "column": 2 }, "end": { "line": 2, "column": 12 } }
        },
        "s": { "0": 5, "1": 0 },
        "fnMap": {
          "0": { "name": "add", "decl": { "start": { "line": 1, "column": 0 }, "end": { "line": 1, "column": 3 } } }
        },
        "f": { "0": 5 },
        "branchMap": {
          "0": { "loc": { "start": { "line": 2, "column": 2 }, "end": { "line": 2, "column": 20 } } }
        },
        "b": { "0": [5, 0] }
      }
    }"#;

    #[test]
    fn parses_statements_functions_branches() {
        let files = parse(SAMPLE, Path::new("/proj"), &"r".to_string()).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];

        // 1-based statement lines 1,2 -> 0-based 0,1; hits 5 and 0.
        let cov: Vec<_> = f.lines.iter().map(|l| (l.line, l.hits)).collect();
        assert_eq!(cov, vec![(0, 5), (1, 0)]);
        assert_eq!(f.summary.lines_covered, 1);
        assert_eq!(f.summary.lines_total, 2);

        assert_eq!(f.functions.len(), 1);
        assert_eq!(f.functions[0].name, "add");
        assert_eq!(f.functions[0].hits, 5);

        assert_eq!(f.branches.len(), 1);
        assert_eq!(f.branches[0].arms, vec![5, 0]);
    }
}
