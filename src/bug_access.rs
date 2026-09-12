//! Who may see and change a Linux kernel bug.
//!
//! Authority over a bug comes from three independent sources, composed by
//! taking the strongest: the capability lists in the configuration, the
//! MAINTAINERS file, and, for operators, the loopback bypass handled at the
//! HTTP edge. This module owns the first two and the composition; it does not
//! know about HTTP.

use crate::maintainers::MaintainersIndex;
use crate::settings::AclSettings;
use std::collections::HashSet;

/// A MAINTAINERS section title, normalized for comparison.
///
/// The type exists so that it is a compile error to compare a section title
/// against a directory prefix. Both are strings that name a subsystem, and
/// conflating them is the single most likely way to get this wrong: a
/// directory prefix is derived from the touched paths and names nobody, while
/// a section title names the people the kernel trusts with that code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SectionTitle(String);

impl SectionTitle {
    /// Normalizes a title by trimming, collapsing internal whitespace and
    /// lowercasing, so that a title stored on a bug matches the same title
    /// read out of MAINTAINERS regardless of incidental formatting.
    pub fn new(title: &str) -> Self {
        let mut normalized = String::with_capacity(title.len());
        for word in title.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.extend(word.chars().flat_map(char::to_lowercase));
        }
        Self(normalized)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a principal may do to one specific bug.
///
/// Ordered from least to most authority, so that composing two grants over the
/// same bug is a maximum and invalid combinations cannot be represented. A
/// security list member who also maintains the affected subsystem gets Manage,
/// not Comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BugAccess {
    /// The bug is invisible. Reads answer as though it does not exist, so that
    /// its existence is not itself disclosed.
    None,
    /// May read the bug, its report, its enrichments and its comments.
    /// Deliberately excludes the raw analysis transcripts, which disclose
    /// other bugs; see [`BugPrincipal::has_global_bug_visibility`].
    Read,
    /// Read, plus may attach a comment.
    Comment,
    /// Comment, plus may close, dismiss, assign and mark duplicate.
    ///
    /// A future re-analysis endpoint belongs here rather than at Comment,
    /// because re-running an analysis spends money and the security list must
    /// not be able to trigger it outside its own maintained subsystems.
    Manage,
}

impl BugAccess {
    /// Whether this level permits reading the bug at all.
    pub fn can_read(self) -> bool {
        self >= BugAccess::Read
    }

    /// Whether this level permits attaching a comment.
    pub fn can_comment(self) -> bool {
        self >= BugAccess::Comment
    }

    /// Whether this level permits changing the bug.
    pub fn can_manage(self) -> bool {
        self >= BugAccess::Manage
    }
}

/// The bug-domain authority of one caller, resolved once per request from the
/// configuration and the MAINTAINERS index.
#[derive(Debug, Clone, Default)]
pub struct BugPrincipal {
    email: String,
    /// Sashiko operator. Manage on every bug.
    operator: bool,
    /// Kernel security list member. Comment on every bug.
    security: bool,
    /// Maintainer of a section that claims the whole tree, which today means
    /// THE REST. Manage on every bug.
    global_maintainer: bool,
    /// The sections this address maintains. Manage within these.
    maintained_sections: HashSet<SectionTitle>,
    /// May file a new bug over HTTP.
    may_create: bool,
}

impl BugPrincipal {
    /// A caller who proved nothing. Holds no authority over any bug.
    pub fn anonymous() -> Self {
        Self::default()
    }

    /// Resolves what an authenticated address may do.
    ///
    /// A blocklisted address resolves to the anonymous principal, so that
    /// every later question about it answers no without the caller having to
    /// remember to ask about the blocklist first.
    pub fn resolve(email: &str, acl: &AclSettings, maintainers: Option<&MaintainersIndex>) -> Self {
        if email.trim().is_empty() || acl.is_blocklisted(email) {
            return Self::anonymous();
        }
        let operator = acl.is_admin(email);
        let maintained_sections = maintainers
            .and_then(|index| index.subsystems_for_address(email))
            .map(|titles| titles.iter().map(|t| SectionTitle::new(t)).collect())
            .unwrap_or_default();
        Self {
            email: email.trim().to_string(),
            operator,
            security: acl.is_security(email),
            global_maintainer: maintainers.is_some_and(|index| index.is_global_maintainer(email)),
            maintained_sections,
            // Operators can always file bugs; the list exists to admit tools
            // in addition to them.
            may_create: operator || acl.is_bug_reporter(email),
        }
    }

    /// The address this principal was resolved from.
    pub fn email(&self) -> &str {
        &self.email
    }

    /// Resolves the access level for a bug from the subsystems attributed to
    /// it.
    ///
    /// Only titles matched out of MAINTAINERS may be passed here. A directory
    /// prefix or a name the reporter invented names nobody, so a bug carrying
    /// only those matches no maintainer and stays invisible to them, which is
    /// the intended outcome for a bug nobody could classify.
    pub fn access_to(&self, attributed: &[SectionTitle]) -> BugAccess {
        if self.operator || self.global_maintainer {
            return BugAccess::Manage;
        }
        if attributed
            .iter()
            .any(|title| self.maintained_sections.contains(title))
        {
            return BugAccess::Manage;
        }
        if self.security {
            return BugAccess::Comment;
        }
        BugAccess::None
    }

    /// Whether this principal can read every bug regardless of subsystem.
    ///
    /// This gates the raw analysis transcripts. A transcript embeds the
    /// problem statements of unrelated bugs, because the deduplication stage
    /// compares the bug against every other bug in the database, so serving
    /// one to a subsystem-scoped maintainer would leak across the boundary the
    /// rest of the model maintains. For a principal who can already read every
    /// bug it discloses nothing new.
    pub fn has_global_bug_visibility(&self) -> bool {
        self.operator || self.security || self.global_maintainer
    }

    /// Whether this principal may file a new bug.
    pub fn may_create(&self) -> bool {
        self.may_create
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MAINTAINERS: &str = r#"
Maintainers List
===================

BTRFS FILE SYSTEM
M:	Chris Mason <clm@fb.com>
S:	Maintained
F:	fs/btrfs/

NETWORKING [GENERAL]
M:	David S. Miller <davem@davemloft.net>
S:	Maintained
F:	net/

THE REST
M:	Linus Torvalds <torvalds@linux-foundation.org>
S:	Buried alive in reporters
F:	*
F:	*/
"#;

    fn index() -> MaintainersIndex {
        MaintainersIndex::from_reader(SAMPLE_MAINTAINERS.as_bytes()).unwrap()
    }

    fn acl() -> AclSettings {
        AclSettings {
            admins: vec!["operator@example.org".to_string()],
            security: vec!["security@example.org".to_string()],
            bug_reporters: vec!["tool@example.org".to_string()],
            blocklist: vec!["clm@fb.com".to_string()],
            ..Default::default()
        }
    }

    fn btrfs() -> Vec<SectionTitle> {
        vec![SectionTitle::new("BTRFS FILE SYSTEM")]
    }

    #[test]
    fn test_section_title_ignores_incidental_formatting() {
        assert_eq!(
            SectionTitle::new("  BTRFS   FILE\tSYSTEM "),
            SectionTitle::new("btrfs file system")
        );
        assert_ne!(
            SectionTitle::new("BTRFS FILE SYSTEM"),
            SectionTitle::new("fs/btrfs")
        );
    }

    #[test]
    fn test_maintainer_manages_own_subsystem_only() {
        let index = index();
        let acl = AclSettings::default();
        let davem = BugPrincipal::resolve("davem@davemloft.net", &acl, Some(&index));
        assert_eq!(
            davem.access_to(&[SectionTitle::new("NETWORKING [GENERAL]")]),
            BugAccess::Manage
        );
        // Someone else's subsystem is invisible, not merely read only.
        assert_eq!(davem.access_to(&btrfs()), BugAccess::None);
        assert!(!davem.has_global_bug_visibility());
        assert!(!davem.may_create());
    }

    #[test]
    fn test_security_list_comments_everywhere_but_manages_nothing() {
        let index = index();
        let principal = BugPrincipal::resolve("security@example.org", &acl(), Some(&index));
        assert_eq!(principal.access_to(&btrfs()), BugAccess::Comment);
        // Including bugs nobody could attribute to a section at all.
        assert_eq!(principal.access_to(&[]), BugAccess::Comment);
        assert!(principal.has_global_bug_visibility());
        assert!(!principal.may_create());
    }

    #[test]
    fn test_operator_and_catch_all_maintainer_manage_everything() {
        let index = index();
        for email in ["operator@example.org", "torvalds@linux-foundation.org"] {
            let principal = BugPrincipal::resolve(email, &acl(), Some(&index));
            assert_eq!(principal.access_to(&btrfs()), BugAccess::Manage);
            assert_eq!(principal.access_to(&[]), BugAccess::Manage);
            assert!(principal.has_global_bug_visibility());
        }
        // Only the operator may file bugs; being Linus is not a reporter role.
        assert!(BugPrincipal::resolve("operator@example.org", &acl(), Some(&index)).may_create());
        assert!(
            !BugPrincipal::resolve("torvalds@linux-foundation.org", &acl(), Some(&index))
                .may_create()
        );
    }

    #[test]
    fn test_strongest_grant_wins() {
        let acl = AclSettings {
            security: vec!["davem@davemloft.net".to_string()],
            ..Default::default()
        };
        let principal = BugPrincipal::resolve("davem@davemloft.net", &acl, Some(&index()));
        // Maintainer of the affected subsystem and on the security list.
        assert_eq!(
            principal.access_to(&[SectionTitle::new("NETWORKING [GENERAL]")]),
            BugAccess::Manage
        );
        // Elsewhere the weaker grant still applies.
        assert_eq!(principal.access_to(&btrfs()), BugAccess::Comment);
    }

    #[test]
    fn test_blocklisted_maintainer_holds_nothing() {
        let principal = BugPrincipal::resolve("clm@fb.com", &acl(), Some(&index()));
        assert_eq!(principal.access_to(&btrfs()), BugAccess::None);
        assert!(!principal.has_global_bug_visibility());
        assert!(!principal.may_create());
        assert_eq!(principal.email(), "");
    }

    #[test]
    fn test_unknown_and_anonymous_callers_see_nothing() {
        for principal in [
            BugPrincipal::anonymous(),
            BugPrincipal::resolve("stranger@example.org", &acl(), Some(&index())),
            // A missing index must not accidentally widen anyone's access.
            BugPrincipal::resolve("davem@davemloft.net", &AclSettings::default(), None),
        ] {
            assert_eq!(principal.access_to(&btrfs()), BugAccess::None);
            assert_eq!(principal.access_to(&[]), BugAccess::None);
            assert!(!principal.has_global_bug_visibility());
            assert!(!principal.may_create());
        }
    }

    #[test]
    fn test_access_levels_are_ordered() {
        assert!(BugAccess::None < BugAccess::Read);
        assert!(BugAccess::Read < BugAccess::Comment);
        assert!(BugAccess::Comment < BugAccess::Manage);
        assert!(!BugAccess::None.can_read());
        assert!(BugAccess::Read.can_read());
        assert!(!BugAccess::Read.can_comment());
        assert!(BugAccess::Comment.can_comment());
        assert!(!BugAccess::Comment.can_manage());
        assert!(BugAccess::Manage.can_manage());
    }
}
