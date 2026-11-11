//! Choosing agents to invite, from the on-chain directory.
//!
//! An agent qualifies when it takes jobs at this reward and either lists a
//! skill the job needs or holds a current badge for one from an issuer the
//! caller trusts. Qualified agents are ranked by what the contract wrote:
//! trusted badges for the job's skills first, then paid wins, then passes,
//! pass rate and fewer bonds lost. Only scores the enclave signed count, so
//! a profile alone earns no rank.

use serde::{Deserialize, Serialize};

/// One directory entry, as `/v1/agents` and `sphinx_agents` give it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub uid: String,
    pub profile: CandidateProfile,
    pub stats: CandidateStats,
    /// Skill badges; absent on contracts without assessments.
    #[serde(default)]
    pub badges: Vec<CandidateBadge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CandidateBadge {
    pub issuer: String,
    pub skill: String,
    #[serde(default)]
    pub current: bool,
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
    /// Paid work jobs at or above the contract's win floor.
    #[serde(default)]
    pub wins: u64,
}

/// An agent worth inviting, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recommendation {
    pub account: String,
    pub uid: String,
    pub name: String,
    pub passes: u64,
    pub fails: u64,
    #[serde(default)]
    pub wins: u64,
    pub matching_skills: Vec<String>,
    /// Skills the agent holds a current trusted badge for.
    #[serde(default)]
    pub badges: Vec<String>,
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

/// A skill as badges spell it: lowercase, words joined by `-`.
fn skill_key(s: &str) -> String {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// At most `count` agents for a job needing `skills` at `reward` yoctoNEAR,
/// best first. Badges count only from `issuers`.
pub fn recommend(
    candidates: &[Candidate],
    skills: &[String],
    reward: u128,
    count: usize,
    issuers: &[String],
) -> Vec<Recommendation> {
    let wanted: Vec<String> = skills.iter().map(|s| skill_key(s)).collect();
    let mut ranked: Vec<(&Candidate, Vec<String>, Vec<String>)> = candidates
        .iter()
        .filter(|c| yocto(&c.profile.min_reward) <= reward)
        .map(|c| {
            let matching: Vec<String> = c
                .profile
                .skills
                .iter()
                .map(|s| skill_key(s))
                .filter(|s| wanted.contains(s))
                .collect();
            let mut badges: Vec<String> = c
                .badges
                .iter()
                .filter(|b| b.current && issuers.contains(&b.issuer))
                .map(|b| skill_key(&b.skill))
                .filter(|s| wanted.contains(s))
                .collect();
            badges.dedup();
            (c, matching, badges)
        })
        .filter(|(_, matching, badges)| !matching.is_empty() || !badges.is_empty())
        .collect();
    ranked.sort_by(|(a, am, ab), (b, bm, bb)| {
        bb.len()
            .cmp(&ab.len())
            .then(b.stats.wins.cmp(&a.stats.wins))
            .then(b.stats.passes.cmp(&a.stats.passes))
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
        .map(|(c, matching, badges)| {
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
                wins: c.stats.wins,
                reason: reason(&record, c.stats.wins, &badges, &matching),
                matching_skills: matching,
                badges,
            }
        })
        .collect()
}

/// One plain sentence: badges, then wins, then the record and listed skills.
fn reason(record: &str, wins: u64, badges: &[String], matching: &[String]) -> String {
    let mut parts = Vec::new();
    if !badges.is_empty() {
        parts.push(format!("passed the {} assessment", badges.join(", ")));
    }
    if wins > 0 {
        parts.push(format!("{wins} paid {}", if wins == 1 { "win" } else { "wins" }));
    }
    parts.push(record.to_owned());
    if !matching.is_empty() {
        parts.push(format!("knows {}", matching.join(", ")));
    }
    parts.join("; ")
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
        let picks = recommend(&all, &["javascript".into()], 500, 3, &[]);
        let names: Vec<&str> = picks.iter().map(|r| r.account.as_str()).collect();
        assert_eq!(names, ["strong", "weak", "new"]);
        assert_eq!(picks[0].reason, "11 of 11 scored submissions passed (100%); knows javascript");
        assert_eq!(picks[2].reason, "no scored submissions yet; knows javascript");
        assert_eq!(recommend(&all, &["javascript".into()], 500, 1, &[]).len(), 1);
    }

    #[test]
    fn trusted_badges_then_wins_rank_first_and_others_are_ignored() {
        let mut badged = agent("badged", &[], "0", 1, 3);
        badged.badges = serde_json::from_value(json!([
            { "issuer": "sphinx.testnet", "skill": "data-parsing", "current": true }
        ]))
        .unwrap();
        let mut self_issued = agent("self", &["data parsing"], "0", 0, 0);
        self_issued.badges = serde_json::from_value(json!([
            { "issuer": "self.testnet", "skill": "data-parsing", "current": true }
        ]))
        .unwrap();
        let mut expired = agent("expired", &["data parsing"], "0", 0, 0);
        expired.badges = serde_json::from_value(json!([
            { "issuer": "sphinx.testnet", "skill": "data-parsing", "current": false }
        ]))
        .unwrap();
        let mut winner = agent("winner", &["Data Parsing"], "0", 2, 0);
        winner.stats.wins = 2;
        let veteran = agent("veteran", &["data-parsing"], "0", 9, 0);
        let all = [veteran, winner, expired, self_issued, badged];
        let issuers = ["sphinx.testnet".to_owned()];
        let picks = recommend(&all, &["data-parsing".into()], 500, 5, &issuers);
        let names: Vec<&str> = picks.iter().map(|r| r.account.as_str()).collect();
        assert_eq!(names, ["badged", "winner", "veteran", "expired", "self"]);
        assert_eq!(
            picks[0].reason,
            "passed the data-parsing assessment; 1 of 4 scored submissions passed (25%)"
        );
        assert_eq!(
            picks[1].reason,
            "2 paid wins; 2 of 2 scored submissions passed (100%); knows data-parsing"
        );
    }
}
