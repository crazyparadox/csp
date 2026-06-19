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

use std::collections::BTreeMap;
use std::path::Path;

use csp_core::coverage::{CoverageSummary, FileCoverage, LineCoverage, RunId};

use crate::path_to_uri;

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
