//! LCOV `.info` parser. LCOV is the lingua franca of coverage — `cargo-llvm-cov`,
//! `grcov`, Istanbul, and gcov all emit it — so this one parser backs the Rust,
//! generic, and (optionally) TS adapters.
//!
//! LCOV line/function numbers are **1-based**; CSP positions are 0-based, so every
//! line number is decremented on the way in.

use std::path::Path;

use csp_core::base::{Position, Range};
use csp_core::coverage::{
    BranchCoverage, CoverageSummary, FileCoverage, FunctionCoverage, LineCoverage, RunId,
};

use crate::path_to_uri;

/// Parse an LCOV document into one [`FileCoverage`] per `SF:` record.
pub fn parse(content: &str, root: &Path, run_id: &RunId) -> Vec<FileCoverage> {
    let mut out = Vec::new();
    let mut cur: Option<Builder> = None;

    for line in content.lines() {
        let line = line.trim();
        if line == "end_of_record" {
            if let Some(b) = cur.take() {
                out.push(b.finish(root, run_id));
            }
            continue;
        }
        let Some((tag, rest)) = line.split_once(':') else {
            continue;
        };
        match tag {
            "SF" => cur = Some(Builder::new(rest.to_string())),
            _ => {
                if let Some(b) = cur.as_mut() {
                    b.record(tag, rest);
                }
            }
        }
    }
    // Tolerate a trailing record with no `end_of_record`.
    if let Some(b) = cur.take() {
        out.push(b.finish(root, run_id));
    }
    out
}

/// Accumulates the records of one `SF:` block before producing a `FileCoverage`.
struct Builder {
    source: String,
    lines: Vec<LineCoverage>,
    functions: Vec<FunctionCoverage>,
    /// Pending function declaration lines keyed by name (`FN:` precedes `FNDA:`).
    fn_decl: Vec<(String, u32)>,
    /// Branch hits keyed by 1-based line; arms accumulate in encounter order.
    branches: Vec<(u32, u64)>,
}

impl Builder {
    fn new(source: String) -> Self {
        Self {
            source,
            lines: Vec::new(),
            functions: Vec::new(),
            fn_decl: Vec::new(),
            branches: Vec::new(),
        }
    }

    fn record(&mut self, tag: &str, rest: &str) {
        match tag {
            // DA:<line>,<hits>[,<checksum>]
            "DA" => {
                let mut it = rest.split(',');
                if let (Some(l), Some(h)) = (it.next(), it.next()) {
                    if let (Ok(l), Ok(h)) = (l.parse::<u32>(), h.parse::<u64>()) {
                        self.lines.push(LineCoverage {
                            line: l.saturating_sub(1),
                            hits: h,
                        });
                    }
                }
            }
            // FN:<line>,<name>
            "FN" => {
                if let Some((l, name)) = rest.split_once(',') {
                    if let Ok(l) = l.parse::<u32>() {
                        self.fn_decl.push((name.to_string(), l.saturating_sub(1)));
                    }
                }
            }
            // FNDA:<hits>,<name>
            "FNDA" => {
                if let Some((h, name)) = rest.split_once(',') {
                    if let Ok(h) = h.parse::<u64>() {
                        let line = self
                            .fn_decl
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, l)| *l)
                            .unwrap_or(0);
                        self.functions.push(FunctionCoverage {
                            name: name.to_string(),
                            range: Range::new(Position::new(line, 0), Position::new(line, 0)),
                            hits: h,
                        });
                    }
                }
            }
            // BRDA:<line>,<block>,<branch>,<taken>  (taken is "-" if not executed)
            "BRDA" => {
                let parts: Vec<&str> = rest.split(',').collect();
                if let [l, _block, _branch, taken] = parts.as_slice() {
                    if let Ok(l) = l.parse::<u32>() {
                        let hits = taken.parse::<u64>().unwrap_or(0);
                        self.branches.push((l.saturating_sub(1), hits));
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(self, root: &Path, run_id: &RunId) -> FileCoverage {
        // Collapse branch arms that share a 0-based line into one BranchCoverage.
        let mut branches: Vec<BranchCoverage> = Vec::new();
        for (line, hits) in self.branches {
            match branches.iter_mut().find(|b| b.range.start.line == line) {
                Some(b) => b.arms.push(hits),
                None => branches.push(BranchCoverage {
                    range: Range::whole_line(line),
                    arms: vec![hits],
                }),
            }
        }

        let summary = CoverageSummary {
            lines_covered: self.lines.iter().filter(|l| l.hits > 0).count() as u64,
            lines_total: self.lines.len() as u64,
            branches_covered: branches
                .iter()
                .flat_map(|b| &b.arms)
                .filter(|&&h| h > 0)
                .count() as u64,
            branches_total: branches.iter().map(|b| b.arms.len() as u64).sum(),
            functions_covered: self.functions.iter().filter(|f| f.hits > 0).count() as u64,
            functions_total: self.functions.len() as u64,
        };

        FileCoverage {
            uri: path_to_uri(root, &self.source),
            run_id: run_id.clone(),
            version: None,
            stale: false,
            lines: self.lines,
            branches,
            functions: self.functions,
            summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
TN:
SF:/proj/src/math.rs
FN:8,add
FNDA:3,add
FNF:1
FNH:1
BRDA:9,0,0,3
BRDA:9,0,1,0
DA:9,3
DA:10,0
LF:2
LH:1
end_of_record
";

    #[test]
    fn parses_lines_functions_branches() {
        let root = Path::new("/proj");
        let files = parse(SAMPLE, root, &"run-1".to_string());
        assert_eq!(files.len(), 1);
        let f = &files[0];

        // 1-based DA:9/DA:10 become 0-based lines 8 and 9.
        assert_eq!(f.lines, vec![
            LineCoverage { line: 8, hits: 3 },
            LineCoverage { line: 9, hits: 0 },
        ]);
        assert_eq!(f.summary.lines_covered, 1);
        assert_eq!(f.summary.lines_total, 2);

        // One function `add`, hit, declared on 0-based line 7.
        assert_eq!(f.functions.len(), 1);
        assert_eq!(f.functions[0].name, "add");
        assert_eq!(f.functions[0].hits, 3);
        assert_eq!(f.functions[0].range.start.line, 7);

        // Two branch arms on 0-based line 8, collapsed into one branch point.
        assert_eq!(f.branches.len(), 1);
        assert_eq!(f.branches[0].arms, vec![3, 0]);
        assert_eq!(f.summary.branches_covered, 1);
        assert_eq!(f.summary.branches_total, 2);
    }
}
