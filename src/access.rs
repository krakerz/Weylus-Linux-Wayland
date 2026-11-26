//! Who may connect: allowed clients, device approval, blocked devices and access code lockout.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::{Duration, Instant};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Wrong access codes from one address that trigger a lockout.
pub const MAX_FAILURES: usize = 5;
/// Window in which `MAX_FAILURES` wrong codes count.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// How long an address stays locked out.
pub const LOCKOUT: Duration = Duration::from_secs(5 * 60);
/// How long a "Deny" answer suppresses new prompts for that address.
pub const DENY_MEMORY: Duration = Duration::from_secs(5 * 60);
/// How long a prompt waits for an answer before the device is refused.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Splits the allowed clients text of the desktop window or CLI into entries.
pub fn split_entries(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseAllowListError {
    entry: String,
}

impl fmt::Display for ParseAllowListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{}' is not an IP address or CIDR range", self.entry)
    }
}

impl std::error::Error for ParseAllowListError {}

/// IP addresses and CIDR ranges that may connect.
#[derive(Clone, Debug, Default)]
pub struct AllowList(Vec<IpNet>);

impl AllowList {
    pub fn parse<'a>(
        entries: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, ParseAllowListError> {
        let nets = entries
            .into_iter()
            .map(|entry| {
                let entry = entry.trim();
                IpNet::from_str(entry)
                    .or_else(|_| IpAddr::from_str(entry).map(IpNet::from))
                    .map_err(|_| ParseAllowListError {
                        entry: entry.to_string(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(nets))
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.0.iter().any(|net| net.contains(&ip))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push(&mut self, ip: IpAddr) {
        let ip = ip.to_canonical();
        if !self.contains(ip) {
            self.0.push(IpNet::from(ip));
        }
    }
}

/// Whether unknown devices are asked about (desktop window) or just refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    Ask,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// Until the server stops.
    Once,
    /// Added to the allowed clients.
    Always,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    LockedOut,
    Blocked,
    DeniedByUser,
    NotAllowed,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Rejection::LockedOut => "locked out after wrong access codes",
            Rejection::Blocked => "blocked until the server stops",
            Rejection::DeniedByUser => "denied in the desktop window",
            Rejection::NotAllowed => "not in the allowed clients",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Allow,
    Ask,
    Deny(Rejection),
}

#[derive(Debug, Default)]
struct Lockout {
    failures: HashMap<IpAddr, Vec<Instant>>,
    locked_until: HashMap<IpAddr, Instant>,
}

impl Lockout {
    fn prune(&mut self, now: Instant) {
        self.locked_until.retain(|_, until| *until > now);
        self.failures.retain(|_, times| {
            times.retain(|t| now.saturating_duration_since(*t) < FAILURE_WINDOW);
            !times.is_empty()
        });
    }

    fn is_locked(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.prune(now);
        self.locked_until.contains_key(&ip)
    }

    fn record(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.prune(now);
        let times = self.failures.entry(ip).or_default();
        times.push(now);
        if times.len() >= MAX_FAILURES {
            self.failures.remove(&ip);
            self.locked_until.insert(ip, now + LOCKOUT);
            true
        } else {
            false
        }
    }
}

/// Admission decisions for incoming connections. Addresses are canonicalised, so
/// IPv4-mapped IPv6 addresses match their IPv4 form.
#[derive(Debug)]
pub struct AccessControl {
    allow: AllowList,
    prompt: Prompt,
    approved: HashSet<IpAddr>,
    blocked: HashSet<IpAddr>,
    denied_until: HashMap<IpAddr, Instant>,
    lockout: Lockout,
}

impl AccessControl {
    pub fn new(allow: AllowList, prompt: Prompt) -> Self {
        Self {
            allow,
            prompt,
            approved: HashSet::new(),
            blocked: HashSet::new(),
            denied_until: HashMap::new(),
            lockout: Lockout::default(),
        }
    }

    pub fn admission(&mut self, ip: IpAddr, now: Instant) -> Admission {
        let ip = ip.to_canonical();
        self.denied_until.retain(|_, until| *until > now);
        if self.lockout.is_locked(ip, now) {
            return Admission::Deny(Rejection::LockedOut);
        }
        if self.blocked.contains(&ip) {
            return Admission::Deny(Rejection::Blocked);
        }
        if self.denied_until.contains_key(&ip) {
            return Admission::Deny(Rejection::DeniedByUser);
        }
        if ip.is_loopback() || self.allow.contains(ip) || self.approved.contains(&ip) {
            return Admission::Allow;
        }
        if self.prompt == Prompt::Ask {
            Admission::Ask
        } else if self.allow.is_empty() {
            Admission::Allow
        } else {
            Admission::Deny(Rejection::NotAllowed)
        }
    }

    pub fn approve(&mut self, ip: IpAddr, approval: Approval, now: Instant) {
        let ip = ip.to_canonical();
        match approval {
            Approval::Once => {
                self.blocked.remove(&ip);
                self.approved.insert(ip);
            }
            Approval::Always => {
                self.blocked.remove(&ip);
                self.allow.push(ip);
                self.approved.insert(ip);
            }
            Approval::Deny => {
                self.denied_until.insert(ip, now + DENY_MEMORY);
            }
        }
    }

    /// Refuses `ip` until the server stops, overriding approvals and the allowed clients.
    /// Returns whether the allowed clients list covers it (the entry stays; the block wins).
    pub fn block(&mut self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.approved.remove(&ip);
        self.blocked.insert(ip);
        self.allow.contains(ip)
    }

    pub fn is_locked_out(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.lockout.is_locked(ip.to_canonical(), now)
    }

    /// Returns true when this failure locked the address.
    pub fn record_failure(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.lockout.record(ip.to_canonical(), now)
    }

    pub fn clear_failures(&mut self, ip: IpAddr) {
        self.lockout.failures.remove(&ip.to_canonical());
    }
}

/// Compares in constant time. Both sides are hashed first so the comparison runs over
/// equal-length input and the length of the access code doesn't leak.
pub fn access_code_matches(given: &str, expected: &str) -> bool {
    let given = Sha256::digest(given.as_bytes());
    let expected = Sha256::digest(expected.as_bytes());
    let diff = given
        .iter()
        .zip(expected.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn list(entries: &[&str]) -> AllowList {
        AllowList::parse(entries.iter().copied()).unwrap()
    }

    #[test]
    fn parse_accepts_addresses_and_ranges() {
        for entry in ["192.168.1.5", "192.168.1.0/24", "fe80::/10", "::1"] {
            assert!(AllowList::parse([entry]).is_ok(), "{entry}");
        }
    }

    #[test]
    fn split_entries_splits_on_commas_and_whitespace() {
        let entries = split_entries(" 10.0.0.1, 10.0.0.0/8\n192.168.1.0/24 ,,");
        assert_eq!(entries, ["10.0.0.1", "10.0.0.0/8", "192.168.1.0/24"]);
    }

    #[test]
    fn parse_errors_name_the_entry() {
        for entry in ["192.168.1.0/33", "foo", "300.1.1.1"] {
            let err = AllowList::parse(["10.0.0.1", entry]).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("'{entry}' is not an IP address or CIDR range")
            );
        }
    }

    #[test]
    fn contains_matches_ranges() {
        let l = list(&["192.168.1.0/24", "fe80::/10"]);
        assert!(l.contains(ip("192.168.1.77")));
        assert!(!l.contains(ip("192.168.2.1")));
        assert!(l.contains(ip("fe80::1c2d")));
        assert!(!l.contains(ip("2001:db8::1")));
        assert!(l.contains(ip("::ffff:192.168.1.7")));
        assert!(list(&["192.168.1.5/24"]).contains(ip("192.168.1.200")));
    }

    #[test]
    fn push_skips_covered_addresses() {
        let mut l = list(&["10.0.0.0/8"]);
        l.push(ip("10.1.2.3"));
        assert_eq!(l.0.len(), 1);
        l.push(ip("::ffff:192.168.1.9"));
        assert!(l.contains(ip("192.168.1.9")));
    }

    #[test]
    fn admission_without_list() {
        let now = Instant::now();
        let mut a = AccessControl::new(AllowList::default(), Prompt::Never);
        assert_eq!(a.admission(ip("10.1.1.1"), now), Admission::Allow);
        let mut a = AccessControl::new(AllowList::default(), Prompt::Ask);
        assert_eq!(a.admission(ip("10.1.1.1"), now), Admission::Ask);
    }

    #[test]
    fn admission_with_list() {
        let now = Instant::now();
        let mut a = AccessControl::new(list(&["192.168.1.0/24"]), Prompt::Never);
        assert_eq!(
            a.admission(ip("10.1.1.1"), now),
            Admission::Deny(Rejection::NotAllowed)
        );
        assert_eq!(a.admission(ip("192.168.1.9"), now), Admission::Allow);
        for lo in ["127.0.0.1", "127.0.0.2", "::1", "::ffff:127.0.0.1"] {
            assert_eq!(a.admission(ip(lo), now), Admission::Allow, "{lo}");
        }
    }

    #[test]
    fn approvals() {
        let now = Instant::now();
        let mut a = AccessControl::new(list(&["192.168.1.0/24"]), Prompt::Ask);
        let once = ip("10.0.0.1");
        assert_eq!(a.admission(once, now), Admission::Ask);
        a.approve(once, Approval::Once, now);
        assert_eq!(a.admission(once, now), Admission::Allow);
        assert!(!a.allow.contains(once));

        let always = ip("10.0.0.2");
        a.approve(always, Approval::Always, now);
        assert_eq!(a.admission(always, now), Admission::Allow);
        assert!(a.allow.contains(always));

        let denied = ip("10.0.0.3");
        a.approve(denied, Approval::Deny, now);
        assert_eq!(
            a.admission(denied, now),
            Admission::Deny(Rejection::DeniedByUser)
        );
        assert_eq!(
            a.admission(denied, now + DENY_MEMORY - Duration::from_secs(1)),
            Admission::Deny(Rejection::DeniedByUser)
        );
        assert_eq!(a.admission(denied, now + DENY_MEMORY), Admission::Ask);
    }

    #[test]
    fn blocked_ips_are_refused_whatever_else_allows_them() {
        let now = Instant::now();
        let mut a = AccessControl::new(list(&["192.168.1.0/24"]), Prompt::Ask);
        let approved = ip("10.0.0.1");
        a.approve(approved, Approval::Once, now);
        assert!(!a.block(approved));
        assert_eq!(
            a.admission(approved, now),
            Admission::Deny(Rejection::Blocked)
        );

        // in the allowed clients: still blocked, and the caller learns about the list
        let listed = ip("192.168.1.9");
        assert!(a.block(listed));
        assert_eq!(
            a.admission(listed, now),
            Admission::Deny(Rejection::Blocked)
        );
        // other members of the range are unaffected
        assert_eq!(a.admission(ip("192.168.1.10"), now), Admission::Allow);

        // the mapped form is the same device
        assert_eq!(
            a.admission(ip("::ffff:10.0.0.1"), now),
            Admission::Deny(Rejection::Blocked)
        );
        // no timeout
        assert_eq!(
            a.admission(approved, now + Duration::from_secs(24 * 3600)),
            Admission::Deny(Rejection::Blocked)
        );
    }

    #[test]
    fn blocked_without_a_list_or_prompt_is_still_refused() {
        let now = Instant::now();
        let mut a = AccessControl::new(AllowList::default(), Prompt::Never);
        a.block(ip("10.0.0.1"));
        assert_eq!(
            a.admission(ip("10.0.0.1"), now),
            Admission::Deny(Rejection::Blocked)
        );
        assert_eq!(a.admission(ip("10.0.0.2"), now), Admission::Allow);
    }

    #[test]
    fn approving_again_lifts_a_block() {
        let now = Instant::now();
        let mut a = AccessControl::new(AllowList::default(), Prompt::Ask);
        a.block(ip("10.0.0.1"));
        a.approve(ip("10.0.0.1"), Approval::Once, now);
        assert_eq!(a.admission(ip("10.0.0.1"), now), Admission::Allow);
    }

    #[test]
    fn lockout_after_five_failures() {
        let now = Instant::now();
        let mut a = AccessControl::new(AllowList::default(), Prompt::Never);
        let who = ip("10.0.0.1");
        for _ in 0..4 {
            assert!(!a.record_failure(who, now));
        }
        assert!(!a.is_locked_out(who, now));
        assert!(a.record_failure(who, now));
        assert_eq!(a.admission(who, now), Admission::Deny(Rejection::LockedOut));
        assert!(a.is_locked_out(who, now + LOCKOUT - Duration::from_secs(1)));
        assert!(!a.is_locked_out(who, now + LOCKOUT));
        assert_eq!(a.admission(who, now + LOCKOUT), Admission::Allow);
    }

    #[test]
    fn lockout_applies_to_loopback() {
        let now = Instant::now();
        let mut a = AccessControl::new(AllowList::default(), Prompt::Never);
        for _ in 0..MAX_FAILURES {
            a.record_failure(ip("127.0.0.1"), now);
        }
        assert_eq!(
            a.admission(ip("127.0.0.1"), now),
            Admission::Deny(Rejection::LockedOut)
        );
    }

    #[test]
    fn spread_out_failures_dont_lock() {
        let now = Instant::now();
        let mut a = AccessControl::new(AllowList::default(), Prompt::Never);
        let who = ip("10.0.0.1");
        let step = FAILURE_WINDOW / 2;
        for i in 0..MAX_FAILURES as u32 {
            assert!(!a.record_failure(who, now + step * i));
        }
    }

    #[test]
    fn clear_failures_resets_the_count() {
        let now = Instant::now();
        let mut a = AccessControl::new(AllowList::default(), Prompt::Never);
        let who = ip("10.0.0.1");
        for _ in 0..4 {
            a.record_failure(who, now);
        }
        a.clear_failures(who);
        assert!(!a.record_failure(who, now));
    }

    #[test]
    fn access_code_comparison() {
        assert!(access_code_matches("secret", "secret"));
        assert!(!access_code_matches("secret", "Secret"));
        assert!(!access_code_matches("abc", "abcd"));
        assert!(!access_code_matches("", "abc"));
        assert!(access_code_matches("", ""));
    }
}
