//! Certificate chains: from a signer's certificate through intermediate
//! authorities to one this computer trusts (RFC 5280, section 6, for what
//! e-mail needs).
//!
//! Every certificate in the chain must be valid now and signed by the next
//! (SHA-1 signatures are refused); authorities must be marked as such
//! (basic constraints, the certificate-signing key usage), respect path
//! length limits and, when they restrict extended key usages, allow e-mail.
//! Name constraints are applied to the addresses (rfc822Name), domain names,
//! directory names and IP addresses below them; any other constrained name
//! form fails. Unknown critical extensions fail. Revocation is not checked.

use x509_cert::ext::pkix::constraints::name::GeneralSubtrees;
use x509_cert::ext::pkix::name::GeneralName;

use crate::cert::{Certificate, Validity};

/// The authorities trusted to vouch for certificates.
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    anchors: Vec<Certificate>,
}

impl TrustStore {
    /// From DER certificates; unreadable ones are left out.
    pub fn new<I, B>(certificates: I) -> TrustStore
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        TrustStore {
            anchors: certificates
                .into_iter()
                .filter_map(|der| Certificate::from_der(der.as_ref()).ok())
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    fn contains(&self, cert: &Certificate) -> bool {
        self.anchors.iter().any(|a| a == cert)
    }
}

/// Whether a certificate is trusted, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    Trusted,
    /// No chain leads to an authority this computer trusts.
    UnknownIssuer,
    /// An authority's certificate in the chain has expired (its name).
    IssuerExpired(String),
    /// The chain breaks a rule (in English, for the details).
    Invalid(String),
}

/// How many authorities may stand between a certificate and its root.
const MAX_DEPTH: usize = 8;

/// Finds a trusted chain for `cert` among `pool` (intermediates from the
/// message and remembered ones). Returns the issuers, the root last.
pub(crate) fn verify(
    cert: &Certificate,
    pool: &[&Certificate],
    trust: &TrustStore,
    now: i64,
) -> Result<Vec<Certificate>, Trust> {
    // A certificate the user's system trusts directly.
    if trust.contains(cert) {
        return Ok(Vec::new());
    }
    if let Some(oid) = cert.unknown_critical {
        return Err(Trust::Invalid(format!(
            "the certificate has a critical extension Envelope does not know ({oid})"
        )));
    }
    let mut search = Search {
        pool,
        trust,
        now,
        problem: None,
        checks: 200,
    };
    let mut path = vec![cert];
    if search.extend(&mut path) {
        Ok(path[1..].iter().map(|c| (*c).clone()).collect())
    } else {
        Err(search.problem.unwrap_or(Trust::UnknownIssuer))
    }
}

struct Search<'a> {
    pool: &'a [&'a Certificate],
    trust: &'a TrustStore,
    now: i64,
    /// The most telling problem met on the way.
    problem: Option<Trust>,
    /// Signature checks left (against loops and certificate bombs).
    checks: usize,
}

impl<'a> Search<'a> {
    fn note(&mut self, problem: Trust) {
        // A broken or expired chain says more than "unknown issuer".
        if self.problem.is_none() || matches!(self.problem, Some(Trust::UnknownIssuer)) {
            self.problem = Some(problem);
        }
    }

    /// Whether `issuer` may have issued `cert`: same name, and the same key
    /// identifier when both say.
    fn names_match(cert: &Certificate, issuer: &Certificate) -> bool {
        cert.issuer_der() == issuer.subject_der()
            && match (&cert.aki, issuer.subject_key_id()) {
                (Some(aki), Some(ski)) => aki.as_slice() == ski,
                _ => true,
            }
    }

    fn signature_ok(&mut self, cert: &Certificate, issuer: &Certificate) -> bool {
        if self.checks == 0 {
            return false;
        }
        self.checks -= 1;
        match cert.signed_by(&issuer.key) {
            Ok(()) => true,
            Err(e) => {
                self.note(Trust::Invalid(format!(
                    "the certificate of {} is not validly signed by {}: {e}",
                    cert.name(),
                    issuer.name()
                )));
                false
            }
        }
    }

    /// Extends `path` (the signer's certificate first) up to a trust anchor.
    fn extend(&mut self, path: &mut Vec<&'a Certificate>) -> bool {
        let current = path[path.len() - 1];
        for anchor in self.trust.anchors.iter() {
            if !Self::names_match(current, anchor) || !self.signature_ok(current, anchor) {
                continue;
            }
            path.push(anchor);
            match check_path(path) {
                Ok(()) => return true,
                Err(problem) => {
                    self.note(problem);
                    path.pop();
                }
            }
        }
        if path.len() > MAX_DEPTH {
            self.note(Trust::Invalid(
                "the chain of certificates is too long".into(),
            ));
            return false;
        }
        for &issuer in self.pool {
            if !Self::names_match(current, issuer) || path.contains(&issuer) {
                continue;
            }
            if let Err(problem) = issuer_ok(issuer, self.now) {
                self.note(problem);
                continue;
            }
            if !self.signature_ok(current, issuer) {
                continue;
            }
            path.push(issuer);
            if self.extend(path) {
                return true;
            }
            path.pop();
        }
        false
    }
}

/// What an intermediate authority must be by itself.
fn issuer_ok(cert: &Certificate, now: i64) -> Result<(), Trust> {
    match cert.validity_at(now) {
        Validity::Valid => {}
        Validity::Expired => return Err(Trust::IssuerExpired(cert.name())),
        Validity::NotYetValid => {
            return Err(Trust::Invalid(format!(
                "the certificate of {} is not valid yet",
                cert.name()
            )));
        }
    }
    if !cert.may_certify() {
        return Err(Trust::Invalid(format!(
            "{} is not a certificate authority",
            cert.name()
        )));
    }
    if !cert.eku_allows_email() {
        return Err(Trust::Invalid(format!(
            "{} may not certify e-mail certificates",
            cert.name()
        )));
    }
    if let Some(oid) = cert.unknown_critical {
        return Err(Trust::Invalid(format!(
            "the certificate of {} has a critical extension Envelope does not know ({oid})",
            cert.name()
        )));
    }
    Ok(())
}

/// The rules that look at the whole path (signer first, root last): path
/// lengths and name constraints.
fn check_path(path: &[&Certificate]) -> Result<(), Trust> {
    for (k, ca) in path.iter().enumerate().skip(1) {
        // Authorities between this one and the signer.
        let below = k - 1;
        if let Some(limit) = ca.basic.as_ref().and_then(|b| b.path_len_constraint)
            && below > limit as usize
        {
            return Err(Trust::Invalid(format!(
                "{} allows {limit} authorities below it",
                ca.name()
            )));
        }
        if let Some(constraints) = &ca.name_constraints {
            for cert in &path[..k] {
                // Self-issued authorities are only checked when they are
                // the end of the chain (RFC 5280, 6.1.3 (b)).
                if cert.is_self_issued() && !std::ptr::eq(*cert, path[0]) {
                    continue;
                }
                for name in cert.constrained_names() {
                    if !permitted(&name, &constraints.permitted_subtrees)
                        || excluded(&name, &constraints.excluded_subtrees)
                    {
                        return Err(Trust::Invalid(format!(
                            "{} may not certify {}",
                            ca.name(),
                            describe(&name)
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn describe(name: &GeneralName) -> String {
    match name {
        GeneralName::Rfc822Name(n) | GeneralName::DnsName(n) => n.as_str().to_string(),
        GeneralName::DirectoryName(n) => n.to_string(),
        _ => "this name".into(),
    }
}

/// How a name relates to a constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// The constraint is about another name form.
    Other,
    Inside,
    Outside,
    /// The same form, but one Envelope cannot compare: it fails either way.
    Unknown,
}

fn relation(name: &GeneralName, base: &GeneralName) -> Relation {
    let inside = |yes: bool| {
        if yes {
            Relation::Inside
        } else {
            Relation::Outside
        }
    };
    match (name, base) {
        (GeneralName::Rfc822Name(addr), GeneralName::Rfc822Name(c)) => {
            inside(email_matches(addr.as_str(), c.as_str()))
        }
        (GeneralName::DnsName(host), GeneralName::DnsName(c)) => {
            inside(domain_matches(host.as_str(), c.as_str()))
        }
        (GeneralName::DirectoryName(n), GeneralName::DirectoryName(c)) => {
            inside(n.0.len() >= c.0.len() && n.0[..c.0.len()] == c.0[..])
        }
        (GeneralName::IpAddress(ip), GeneralName::IpAddress(c)) => {
            let (ip, c) = (ip.as_bytes(), c.as_bytes());
            inside(
                c.len() == ip.len() * 2
                    && ip
                        .iter()
                        .zip(&c[..ip.len()])
                        .zip(&c[ip.len()..])
                        .all(|((a, b), m)| a & m == b & m),
            )
        }
        (n, c) if std::mem::discriminant(n) == std::mem::discriminant(c) => Relation::Unknown,
        _ => Relation::Other,
    }
}

/// A name is permitted when no subtree of its form is listed, or it lies
/// inside one of them.
fn permitted(name: &GeneralName, subtrees: &Option<GeneralSubtrees>) -> bool {
    let Some(subtrees) = subtrees else {
        return true;
    };
    let mut constrained = false;
    for subtree in subtrees {
        match relation(name, &subtree.base) {
            Relation::Inside => return true,
            Relation::Outside | Relation::Unknown => constrained = true,
            Relation::Other => {}
        }
    }
    !constrained
}

fn excluded(name: &GeneralName, subtrees: &Option<GeneralSubtrees>) -> bool {
    subtrees.as_ref().is_some_and(|subtrees| {
        subtrees.iter().any(|s| {
            matches!(
                relation(name, &s.base),
                Relation::Inside | Relation::Unknown
            )
        })
    })
}

/// rfc822Name constraints (RFC 5280, 4.2.1.10): a mailbox, all mailboxes
/// on a host, or (with a leading dot) on any host in a domain.
fn email_matches(address: &str, constraint: &str) -> bool {
    let address = address.to_ascii_lowercase();
    let constraint = constraint.to_ascii_lowercase();
    if constraint.contains('@') {
        return address == constraint;
    }
    let Some((_, host)) = address.rsplit_once('@') else {
        return false;
    };
    match constraint.strip_prefix('.') {
        Some(_) => host.ends_with(&constraint),
        None => host == constraint,
    }
}

/// dNSName constraints: the domain and its subdomains.
fn domain_matches(host: &str, constraint: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let constraint = constraint.trim_end_matches('.').to_ascii_lowercase();
    if constraint.is_empty() {
        return true;
    }
    match constraint.strip_prefix('.') {
        Some(_) => host.ends_with(&constraint),
        None => host == constraint || host.ends_with(&format!(".{constraint}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_constraints() {
        assert!(email_matches("ann@example.com", "example.com"));
        assert!(email_matches("Ann@Example.COM", "example.com"));
        assert!(!email_matches("ann@mail.example.com", "example.com"));
        assert!(email_matches("ann@mail.example.com", ".example.com"));
        assert!(!email_matches("ann@example.com", ".example.com"));
        assert!(email_matches("ann@example.com", "ann@example.com"));
        assert!(!email_matches("bob@example.com", "ann@example.com"));
        assert!(!email_matches("ann@example.net", "example.com"));
    }

    #[test]
    fn domain_constraints() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(domain_matches("mail.example.com", "example.com"));
        assert!(!domain_matches("badexample.com", "example.com"));
        assert!(domain_matches("a.example.com", ".example.com"));
        assert!(!domain_matches("example.com", ".example.com"));
    }
}
