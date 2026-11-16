//! Fuzzy matching for the project (Cmd+K) and branch (Cmd+B) switchers.

/// Score of `query` as a case-insensitive subsequence of `text`, higher is
/// better; None when some query character is missing. Consecutive matches and
/// matches at word starts (after `/ - _ .` or a space, or at a lower-to-upper
/// case change) score extra, and earlier matches beat later ones.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().collect();
    let mut score = 0;
    let mut qi = 0;
    let mut prev_hit: Option<usize> = None;
    for (i, &c) in t.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if !c.to_lowercase().eq(std::iter::once(q[qi])) {
            continue;
        }
        let mut s = 1;
        let boundary = i == 0
            || matches!(t[i - 1], '/' | '-' | '_' | '.' | ' ')
            || (t[i - 1].is_lowercase() && c.is_uppercase());
        if boundary {
            s += 8;
        }
        if prev_hit == Some(i.wrapping_sub(1)) {
            s += 5;
        }
        if qi == 0 {
            // Prefer the first match early in the text.
            s -= (i as i32).min(10);
        }
        score += s;
        prev_hit = Some(i);
        qi += 1;
    }
    (qi == q.len()).then_some(score)
}

/// Last path component, or the whole path when it has none.
pub fn project_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path)
}

/// Indexes of `paths` matching `query`, best first. The query matches the
/// project name; a query containing `/` also matches the full path (a name
/// match still ranks higher). Long paths contain most letters, so matching
/// them for plain queries would list nearly every project. Ties keep the
/// given (recency) order; an empty query keeps every path in order.
pub fn rank(query: &str, paths: &[String]) -> Vec<usize> {
    let mut hits: Vec<(i32, usize)> = paths
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let by_name = fuzzy_score(query, project_name(p)).map(|s| s + 100);
            let by_path = if query.contains('/') { fuzzy_score(query, p) } else { None };
            by_name.max(by_path).map(|s| (s, i))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Indexes of `names` matching `query` against the whole name, best first.
/// Ties and an empty query keep the given order.
pub fn rank_names(query: &str, names: &[String]) -> Vec<usize> {
    let mut hits: Vec<(i32, usize)> = names
        .iter()
        .enumerate()
        .filter_map(|(i, n)| fuzzy_score(query, n).map(|s| (s, i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}
