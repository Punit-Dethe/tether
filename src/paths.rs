//! Path patterns, kept deliberately small.

/// fnmatch-style match: `*` is any run of characters (including `/`), `?` is
/// one character, everything else is literal. Matches Python's
/// `fnmatch.fnmatchcase`, minus `[...]` classes, which no config needs.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn matches_like_fnmatchcase() {
        assert!(glob_match("docs/*", "docs/a/b.md"));
        assert!(glob_match("README.md", "README.md"));
        assert!(!glob_match("README.md", "src/README.md"));
        assert!(glob_match(
            ".github/workflows/*",
            ".github/workflows/ci.yml"
        ));
        assert!(glob_match("src/*", "src/app/x.tsx"));
        assert!(!glob_match("src/*", "src-tauri/x.rs"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("a*b", "a/c"));
    }
}
