use crate::scan::classify_role;
use crate::tokens::count_tokens;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

struct Patterns {
    func_defs: Vec<Regex>,
    class_defs: Vec<Regex>,
    type_defs: Vec<Regex>,
    exports: Vec<Regex>,
    impl_block: Regex,
    comments: Vec<&'static str>,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        func_defs: vec![
            Regex::new(r"(?m)^\s*(?:pub\s+)?(?:async\s+)?fn\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*(?:export\s+)?(?:async\s+)?function\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*(?:export\s+)?(?:const|let|var)\s+\w+\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\([^)]*\)|\w+)\s*=>").unwrap(),
            Regex::new(r"(?m)^\s*func\s+(?:\([^)]*\)\s*)?\w+\s*\(").unwrap(),
            Regex::new(r"(?m)^\s*def\s+\w+\s*\(").unwrap(),
            Regex::new(r"(?m)^\s*(?:(?:public|private|protected|internal|abstract|final|static|synchronized|override|open|async|export|default)\s+)+[A-Za-z_$][\w$<>\[\], .]*\s+[A-Za-z_$][\w$]*\s*\([^;{]*\)\s*(?:\{|;|throws)").unwrap(),
        ],
        class_defs: vec![
            Regex::new(r"(?m)^\s*(?:export\s+)?(?:abstract\s+)?class\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*(?:export\s+)?(?:interface|type|enum)\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*(?:pub\s+)?(?:struct|trait|enum)\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*type\s+\w+\s+struct").unwrap(),
        ],
        type_defs: vec![
            Regex::new(r"(?m)^\s*(?:export\s+)?type\s+\w+\s*=").unwrap(),
            Regex::new(r"(?m)^\s*:\s*(?:interface|type|enum)\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*@dataclass").unwrap(),
            Regex::new(r"(?m)^\s*(?:pub\s+)?type\s+\w+\s+(?:struct|trait)\b").unwrap(),
            Regex::new(r"(?m)^\s*(?:public\s+)?(?:record|sealed\s+interface)\s+\w+").unwrap(),
        ],
        exports: vec![
            Regex::new(r"(?m)^\s*(?:export\s+)?(?:const|let|var)\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*pub\s+(?:const|static)\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*module\s+\w+").unwrap(),
            Regex::new(r"(?m)^\s*(?:import|from)\s+\w+").unwrap(),
        ],
        impl_block: Regex::new(r"(?m)^\s*impl(?:<[^>]*>)?\s+\w+").unwrap(),
        comments: vec![
            "TODO", "FIXME", "HACK", "XXX", "DEPRECATED", "@deprecated", "NOTE:",
        ],
    })
}

pub fn codemap(path: &str, content: &str, max_lines: usize) -> String {
    let p = patterns();
    let mut out = String::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut count = 0usize;

    for (lineno, line) in content.lines().enumerate() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            continue;
        }
        let is_signature = p.func_defs.iter().chain(p.class_defs.iter()).any(|r| r.is_match(line))
            || p.type_defs.iter().any(|r| r.is_match(line))
            || p.impl_block.is_match(line)
            || p.exports.iter().any(|r| r.is_match(line))
            || p.comments.iter().any(|c| line.contains(c));

        if is_signature {
            let signature = collapse_ws(trimmed);
            let key = signature.clone();
            if emitted.insert(key) {
                out.push_str(&format!("{:>5}: {}\n", lineno + 1, signature));
                count += 1;
                if count >= max_lines {
                    out.push_str("      ... (codemap truncated)\n");
                    break;
                }
            }
        }
    }

    if out.trim().is_empty() {
        let fallback: Vec<&str> = content.lines().take(max_lines).collect();
        out = fallback
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{:>5}: {}\n", i + 1, collapse_ws(l)))
            .collect();
        out.push_str("      ... (no signatures detected, raw head shown)\n");
    }

    format!("// codemap: {path}\n{out}")
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' || c == '\t' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    let trimmed = out.trim_end().to_string();
    if trimmed.len() > 160 {
        let mut t = trimmed;
        t.truncate(157);
        t.push_str("...");
        t
    } else {
        trimmed
    }
}

pub fn slice_of(path: &str, content: &str, start: usize, end: usize) -> String {
    let total = content.lines().count();
    let start = start.max(1).min(total.max(1));
    let end = end.max(start).min(total);
    let mut out = String::new();
    if start > 1 {
        out.push_str(&format!("// ... lines 1-{} omitted ...\n", start - 1));
    }
    for (i, line) in content.lines().enumerate() {
        let lineno = i + 1;
        if lineno < start || lineno > end {
            continue;
        }
        out.push_str(&format!("{:>5}: {}\n", lineno, line));
    }
    if end < total {
        out.push_str(&format!("// ... lines {}-{} omitted ...\n", end + 1, total));
    }
    let _ = path;
    out
}

pub fn first_window(content: &str, budget_tokens: usize) -> (usize, usize) {
    let total = content.lines().count();
    if total == 0 {
        return (1, 1);
    }
    let mut low = 1usize;
    let mut high = total;
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let chunk: String = content.lines().take(mid).collect::<Vec<_>>().join("\n");
        if count_tokens(&chunk) <= budget_tokens {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    (1, low.max(1))
}

pub fn role_of(path: &str) -> crate::model::FileRole {
    classify_role(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_typescript_signatures() {
        let src = "export function login(u: User): Token {\n  return token;\n}\n\nexport const logout = () => {\n  clear();\n};\n";
        let map = codemap("auth.ts", src, 40);
        assert!(map.contains("login"), "{map}");
        assert!(map.contains("logout"), "{map}");
        assert!(!map.contains("return token;"));
    }

    #[test]
    fn extracts_rust_signatures() {
        let src = "pub struct User { name: String }\n\nimpl User {\n    pub fn new() -> Self { User { name: String::new() } }\n}\n\npub trait Auth { fn verify(&self); }\n";
        let map = codemap("user.rs", src, 40);
        assert!(map.contains("struct User"), "{map}");
        assert!(map.contains("impl User"), "{map}");
        assert!(map.contains("trait Auth"), "{map}");
    }

    #[test]
    fn extracts_python_signatures() {
        let src = "class UserService:\n    def login(self, user):\n        return token\n\n@dataclass\nclass Session:\n    pass\n";
        let map = codemap("service.py", src, 40);
        assert!(map.contains("class UserService"), "{map}");
        assert!(map.contains("def login"), "{map}");
        assert!(map.contains("@dataclass"), "{map}");
    }

    #[test]
    fn respects_max_lines() {
        let mut src = String::new();
        for i in 0..200 {
            src.push_str(&format!("export function f{i}() {{}}\n"));
        }
        let map = codemap("big.ts", &src, 10);
        assert!(map.contains("codemap truncated"), "{map}");
        assert!(map.lines().count() <= 12);
    }

    #[test]
    fn falls_back_when_no_signatures() {
        let src = "\n\nplain text file\nno code here\n";
        let map = codemap("notes.md", src, 10);
        assert!(map.contains("no signatures detected"), "{map}");
    }

    #[test]
    fn slice_reports_omitted_ranges() {
        let content = (1..=100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let out = slice_of("f.ts", &content, 40, 50);
        assert!(out.contains("lines 1-39 omitted"), "{out}");
        assert!(out.contains("   40: line 40"), "{out}");
        assert!(out.contains("lines 51-100 omitted"), "{out}");
    }

    #[test]
    fn slice_clamps_out_of_bounds() {
        let content = "a\nb\nc";
        let out = slice_of("f.ts", content, 1, 999);
        assert!(out.contains("    3: c"), "{out}");
    }

    #[test]
    fn first_window_fits_budget() {
        let content = "x".repeat(100_000);
        let (_, end) = first_window(&content, 100);
        assert!((1..=100_000).contains(&end));
    }

    #[test]
    fn handles_crlf_line_endings() {
        let src = "export function a() {\r\n  return 1;\r\n}\r\n";
        let map = codemap("a.ts", src, 10);
        assert!(map.contains("function a"), "{map}");
    }
}