//! Public identifiers for jobs, submissions and agents.
//!
//! A job, submission or agent is addressed in URLs by a fixed identifier in UUID
//! form (version 8, RFC 9562) instead of its sequence number. It is the first
//! 16 bytes of SHA-256 over the contract and the item's numbers, so every
//! component derives the same identifier with no stored mapping. It is not a
//! secret: anyone who knows the numbers can derive it.

use sha2::{Digest, Sha256};

fn uuid(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x80; // version 8
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 9562 variant
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// The public identifier of job `job_id` on `contract`.
pub fn job_uid(contract: &str, job_id: u64) -> String {
    uuid(&format!("sphinx:uid:v1:job:{contract}:{job_id}"))
}

/// The public identifier of submission `sub_id` of job `job_id` on `contract`.
pub fn submission_uid(contract: &str, job_id: u64, sub_id: u32) -> String {
    uuid(&format!("sphinx:uid:v1:submission:{contract}:{job_id}:{sub_id}"))
}

/// The public identifier of agent `account` on `contract`.
pub fn agent_uid(contract: &str, account: &str) -> String {
    uuid(&format!("sphinx:uid:v1:agent:{contract}:{account}"))
}

/// Whether `s` has the shape of an identifier from this module.
pub fn is_uid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_stable_distinct_and_well_formed() {
        let a = job_uid("escrow.sphinx999.testnet", 12);
        assert_eq!(a, job_uid("escrow.sphinx999.testnet", 12));
        assert_ne!(a, job_uid("escrow.sphinx999.testnet", 13));
        assert_ne!(a, job_uid("other.testnet", 12));
        assert_ne!(submission_uid("c.testnet", 1, 0), submission_uid("c.testnet", 1, 1));
        assert_ne!(agent_uid("c.testnet", "a.testnet"), agent_uid("c.testnet", "b.testnet"));
        assert_ne!(agent_uid("c.testnet", "a.testnet"), agent_uid("d.testnet", "a.testnet"));
        for id in
            [a.clone(), submission_uid("c.testnet", 1, 0), agent_uid("c.testnet", "a.testnet")]
        {
            assert!(is_uid(&id), "{id}");
            assert_eq!(&id[14..15], "8", "version 8: {id}");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "variant: {id}");
        }
        assert!(!is_uid("12"));
        assert!(!is_uid("C45A72A3-32C2-43E1-AFFE-6344B89225C9"));
    }
}
