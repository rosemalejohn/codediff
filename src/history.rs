//! Paged commit history.
use crate::model::CommitInfo;
use crate::Result;
use git2::{ErrorCode, Repository, Sort};

pub fn list_commits(repo: &Repository, offset: usize, limit: usize) -> Result<Vec<CommitInfo>> {
    match repo.head() {
        Ok(_) => {}
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e.into()),
    }
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    walk.push_head()?;
    let mut out = Vec::new();
    for oid in walk.skip(offset).take(limit) {
        let oid = oid?;
        let c = repo.find_commit(oid)?;
        let id_str = oid.to_string();
        out.push(CommitInfo {
            id: oid,
            short_id: id_str[..7].to_string(),
            summary: c.message().unwrap_or("").lines().next().unwrap_or("").to_string(),
            author: c.author().name().unwrap_or("").to_string(),
            time: c.time().seconds(),
        });
    }
    Ok(out)
}

pub fn relative_time(now: i64, then: i64) -> String {
    let d = now - then;
    if d < 60 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else if d < 30 * 86400 {
        format!("{}d ago", d / 86400)
    } else {
        let (y, m, day) = civil_from_days(then.div_euclid(86400));
        format!("{y:04}-{m:02}-{day:02}")
    }
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
