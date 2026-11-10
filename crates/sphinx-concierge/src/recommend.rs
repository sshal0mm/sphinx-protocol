//! Choosing agents to invite, from the on-chain directory.
//!
//! An agent qualifies when it takes jobs at this reward and shares a skill
//! the job needs. Qualified agents are ranked by their verified record:
//! passes first, then pass rate, then fewer bonds lost. Only scores the
//! enclave signed count, so a profile alone earns no rank.

use serde::{Deserialize, Serialize};

/// One directory entry, as `/v1/agents` and `sphinx_agents` give it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub uid: String,
    pub profile: CandidateProfile,
    pub stats: CandidateStats,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CandidateProfile {
    pub account: String,
    pub name: String,
    #[serde(default)]
    pub skills: Vec<String>,
    /// yoctoNEAR as a decimal string.
    #[serde(default)]
    pub min_reward: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CandidateStats {
    pub passes: u64,
    pub fails: u64,
    #[serde(default)]
    pub earned: String,
}

/// An agent worth inviting, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recommendation {
    pub account: String,
    pub uid: String,
    pub name: String,
    pub passes: u64,
    pub fails: u64,
    pub matching_skills: Vec<String>,
    /// One plain sentence a buyer can read.
    pub reason: String,
}

fn yocto(s: &str) -> u128 {
    s.parse().unwrap_or(0)
}

/// Pass rate in percent, or `None` before any scored submission.
fn pass_rate(passes: u64, fails: u64) -> Option<u64> {
    let scored = passes + fails;
    (scored > 0).then(|| passes * 100 / scored)
}

/// At most `count` agents for a job needing `skills` at `reward` yoctoNEAR, best first.
pub fn recommend(
    candidates: &[Candidate],
    skills: &[String],
    reward: u128,
    count: usize,
) -> Vec<Recommendation> {
    let wanted: Vec<String> = skills.iter().map(|s| s.to_lowercase()).collect();
    let mut ranked: Vec<(&Candidate, Vec<String>)> = candidates
        .iter()
        .filter(|c| yocto(&c.profile.min_reward) <= reward)
        .map(|c| {
            let matching: Vec<String> = c
                .profile
                .skills
                .iter()
                .map(|s| s.to_lowercase())
                .filter(|s| wanted.contains(s))
                .collect();
            (c, matching)
        })
        .filter(|(_, matching)| !matching.is_empty())
        .collect();
    ranked.sort_by(|(a, am), (b, bm)| {
        b.stats
            .passes
            .cmp(&a.stats.passes)
            .then(
                pass_rate(b.stats.passes, b.stats.fails)
                    .cmp(&pass_rate(a.stats.passes, a.stats.fails)),
            )
            .then(bm.len().cmp(&am.len()))
            .then(a.stats.fails.cmp(&b.stats.fails))
    });
    ranked
        .into_iter()
        .take(count)
        .map(|(c, matching)| {
            let record = match pass_rate(c.stats.passes, c.stats.fails) {
                Some(rate) => format!(
                    "{} of {} scored submissions passed ({rate}%)",
                    c.stats.passes,
                    c.stats.passes + c.stats.fails
                ),
                None => "no scored submissions yet".to_owned(),
            };
            Recommendation {
                account: c.profile.account.clone(),
                uid: c.uid.clone(),
                name: c.profile.name.clone(),
                passes: c.stats.passes,
                fails: c.stats.fails,
                reason: format!("{record}; knows {}", matching.join(", ")),
                matching_skills: matching,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(
        account: &str,
        skills: &[&str],
        min_reward: &str,
        passes: u64,
        fails: u64,
    ) -> Candidate {
        serde_json::from_value(json!({
            "uid": format!("uid-{account}"),
            "profile": { "account": account, "name": account, "skills": skills, "min_reward": min_reward },
            "stats": { "passes": passes, "fails": fails, "earned": "0" }
        }))
        .unwrap()
    }

    #[test]
    fn ranks_by_verified_record_among_agents_that_fit() {
        let all = [
            agent("weak", &["javascript"], "0", 1, 6),
            agent("strong", &["JavaScript", "parsing"], "0", 11, 0),
            agent("new", &["javascript"], "0", 0, 0),
            agent("pricey", &["javascript"], "900", 50, 0),
            agent("other", &["rust"], "0", 40, 0),
        ];
        let picks = recommend(&all, &["javascript".into()], 500, 3);
        let names: Vec<&str> = picks.iter().map(|r| r.account.as_str()).collect();
        assert_eq!(names, ["strong", "weak", "new"]);
        assert_eq!(picks[0].reason, "11 of 11 scored submissions passed (100%); knows javascript");
        assert_eq!(picks[2].reason, "no scored submissions yet; knows javascript");
        assert_eq!(recommend(&all, &["javascript".into()], 500, 1).len(), 1);
    }
}
