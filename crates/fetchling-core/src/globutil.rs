//! Simple glob matching for accept lists and FTP patterns.

pub fn match_glob(name: &str, pat: &str, ignore_case: bool) -> bool {
    let (name, pat) = if ignore_case {
        (name.to_ascii_lowercase(), pat.to_ascii_lowercase())
    } else {
        (name.to_string(), pat.to_string())
    };
    glob_match(&name, &pat)
}

fn glob_match(name: &str, pat: &str) -> bool {
    let nb = name.as_bytes();
    let pb = pat.as_bytes();
    let mut ni = 0usize;
    let mut pi = 0usize;
    let mut star_pi = None;
    let mut star_ni = 0usize;
    while ni < nb.len() {
        if pi < pb.len() && (pb[pi] == b'?' || pb[pi] == nb[ni]) {
            ni += 1;
            pi += 1;
            continue;
        }
        if pi < pb.len() && pb[pi] == b'*' {
            star_pi = Some(pi);
            star_ni = ni;
            pi += 1;
            continue;
        }
        if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ni += 1;
            ni = star_ni;
            continue;
        }
        if pi < pb.len() && pb[pi] == b'[' {
            if let Some((next_pi, ok)) = match_bracket(nb[ni], &pb[pi..]) {
                if ok {
                    ni += 1;
                    pi += next_pi;
                    continue;
                }
            }
            if let Some(sp) = star_pi {
                pi = sp + 1;
                star_ni += 1;
                ni = star_ni;
                continue;
            }
            return false;
        }
        return false;
    }
    while pi < pb.len() && pb[pi] == b'*' {
        pi += 1;
    }
    pi == pb.len()
}

fn match_bracket(ch: u8, pat: &[u8]) -> Option<(usize, bool)> {
    if pat.first() != Some(&b'[') {
        return None;
    }
    let mut i = 1;
    let mut negated = false;
    if pat.get(i) == Some(&b'!') || pat.get(i) == Some(&b'^') {
        negated = true;
        i += 1;
    }
    let mut matched = false;
    while i < pat.len() && pat[i] != b']' {
        if i + 2 < pat.len() && pat[i + 1] == b'-' && pat[i + 2] != b']' {
            let lo = pat[i];
            let hi = pat[i + 2];
            if (lo..=hi).contains(&ch) {
                matched = true;
            }
            i += 3;
        } else {
            if pat[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    if i >= pat.len() || pat[i] != b']' {
        return None;
    }
    Some((i + 1, matched != negated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_and_question() {
        assert!(match_glob("file.txt", "*.txt", false));
        assert!(match_glob("abc", "a?c", false));
        assert!(!match_glob("file.bin", "*.txt", false));
        assert!(match_glob("a1b", "a[0-9]b", false));
        assert!(!match_glob("axb", "a[0-9]b", false));
    }
}
