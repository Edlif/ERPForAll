//! Naming and tagging of throwaway databases.
//!
//! Every test database is named `erpforall_test_<pid>_<n>_<created>`, where `<created>` is the
//! creation time in seconds since the Unix epoch. PostgreSQL keeps no creation time for databases,
//! so the name carries it: the sweeper always knows a database's age, even when its creator died
//! before tagging it, and no lock between creators and the sweeper is needed.
//!
//! Each database also carries a comment (`COMMENT ON DATABASE`) recording the creator's pid and run
//! id, which lets `scripts/ci.sh` drop exactly the databases of its own run.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Result, TestkitError};

/// Prefix shared by every throwaway database. The sweeper never touches anything else.
pub const TEST_DB_PREFIX: &str = "erpforall_test_";

/// Prefix shared by every cached template database created by [`crate::TemplateDb`].
pub const TEMPLATE_DB_PREFIX: &str = "erpforall_tpl_";

/// Environment variable naming the current test run (set by `scripts/ci.sh`).
pub const RUN_ID_ENV: &str = "ERP_TESTKIT_RUN_ID";

const TAG_MAGIC: &str = "erpforall-testkit";
const TEMPLATE_TAG_MAGIC: &str = "erpforall-testkit-tpl";
const TAG_VERSION: &str = "v1";
const NO_RUN_ID: &str = "-";
const TEMPLATE_HEX_LEN: usize = 32;

/// Seconds since the Unix epoch.
pub(crate) fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(TestkitError::Clock)
}

/// The parts of a test database name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestDbName {
    /// Creator's process id.
    pub pid: u32,
    /// Per-process counter.
    pub n: u64,
    /// Creation time, seconds since the Unix epoch.
    pub created_at: u64,
}

impl TestDbName {
    /// Render as `erpforall_test_<pid>_<n>_<created_at>`.
    pub fn render(&self) -> String {
        format!(
            "{TEST_DB_PREFIX}{}_{}_{}",
            self.pid, self.n, self.created_at
        )
    }

    /// Parse a name of exactly that shape; anything else is `None`.
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix(TEST_DB_PREFIX)?;
        let mut parts = rest.split('_');
        let mut number = || -> Option<&str> {
            let part = parts.next()?;
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())).then_some(part)
        };
        let pid = number()?.parse().ok()?;
        let n = number()?.parse().ok()?;
        let created_at = number()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self { pid, n, created_at })
    }
}

/// True when `name` is a test database name this crate could have produced.
pub fn is_test_db_name(name: &str) -> bool {
    TestDbName::parse(name).is_some()
}

/// Deterministic name of a cached template database: `erpforall_tpl_<32 lowercase hex chars>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemplateDbName {
    digest_prefix: [u8; 16],
}

impl TemplateDbName {
    /// Build a template database name from the first 16 bytes of a digest.
    #[must_use]
    pub const fn from_digest_prefix(digest_prefix: [u8; 16]) -> Self {
        Self { digest_prefix }
    }

    /// Render as `erpforall_tpl_<32 lowercase hex chars>` (46 bytes, always a valid PostgreSQL
    /// identifier).
    #[must_use]
    pub fn render(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(TEMPLATE_DB_PREFIX.len() + TEMPLATE_HEX_LEN);
        out.push_str(TEMPLATE_DB_PREFIX);
        for byte in self.digest_prefix {
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        out
    }

    /// Parse a name of exactly `erpforall_tpl_<32 lowercase hex chars>`; anything else is `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix(TEMPLATE_DB_PREFIX)?;
        if rest.len() != TEMPLATE_HEX_LEN {
            return None;
        }
        let bytes = rest.as_bytes();
        let mut digest_prefix = [0u8; 16];
        for (idx, slot) in digest_prefix.iter_mut().enumerate() {
            let hi = hex_nibble(bytes[idx * 2])?;
            let lo = hex_nibble(bytes[idx * 2 + 1])?;
            *slot = (hi << 4) | lo;
        }
        Some(Self { digest_prefix })
    }

    /// PostgreSQL advisory lock key (`pg_advisory_lock(bigint)`) associated with this template
    /// database name. Advisory locks are scoped to the connected admin database, so callers and
    /// `erp-testkit sweep` targeting the same cluster must connect to the same admin database.
    #[must_use]
    pub const fn lock_key(&self) -> i64 {
        let mut head = [0u8; 8];
        let mut i = 0;
        while i < 8 {
            head[i] = self.digest_prefix[i];
            i += 1;
        }
        i64::from_be_bytes(head)
    }
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// True when `name` is a cached template database name this crate could have produced.
#[must_use]
pub fn is_template_db_name(name: &str) -> bool {
    TemplateDbName::parse(name).is_some()
}

/// Validate the caller-supplied logical template key before hashing it.
pub(crate) fn validate_template_key(key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 256
        && key.bytes().all(|b| b.is_ascii_graphic() || b == b' ');
    if ok {
        Ok(())
    } else {
        Err(TestkitError::InvalidName {
            what: "template key",
            value: key.to_owned(),
            rule: "1-256 printable ASCII characters",
        })
    }
}

/// Check a run id before it is written into a database comment.
pub(crate) fn validate_run_id(run_id: &str) -> Result<()> {
    let ok = run_id != NO_RUN_ID
        && !run_id.is_empty()
        && run_id.len() <= 128
        && run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'));
    if ok {
        Ok(())
    } else {
        Err(TestkitError::InvalidName {
            what: "run id",
            value: run_id.to_owned(),
            rule: "1-128 characters from [A-Za-z0-9_.:-], and not \"-\"",
        })
    }
}

/// Check a template database name before it is spliced into `CREATE DATABASE`.
pub(crate) fn validate_template(template: &str) -> Result<()> {
    let mut bytes = template.bytes();
    let first_ok = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
    let ok = first_ok
        && template.len() <= 63
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(TestkitError::InvalidName {
            what: "template database",
            value: template.to_owned(),
            rule: "a lowercase identifier [a-z_][a-z0-9_]*, at most 63 bytes",
        })
    }
}

/// Metadata stored in the database comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Process id of the creator.
    pub pid: u32,
    /// Run id of the creator, if one was set.
    pub run_id: Option<String>,
}

/// What a database comment says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comment {
    /// No comment: the creator died between `CREATE DATABASE` and `COMMENT ON DATABASE`.
    Missing,
    /// A tag this version understands.
    Tag(Tag),
    /// A comment this version cannot read (a future tag version, or someone else's note).
    Unreadable,
}

impl Tag {
    /// Render the tag as the comment text. `run_id` must already be validated.
    pub(crate) fn render(&self) -> String {
        format!(
            "{TAG_MAGIC} {TAG_VERSION} pid={} run_id={}",
            self.pid,
            self.run_id.as_deref().unwrap_or(NO_RUN_ID),
        )
    }

    /// Parse a comment written by `Tag::render`.
    pub fn parse(comment: Option<&str>) -> Comment {
        let Some(comment) = comment else {
            return Comment::Missing;
        };
        Self::parse_v1(comment).map_or(Comment::Unreadable, Comment::Tag)
    }

    fn parse_v1(comment: &str) -> Option<Self> {
        let mut parts = comment.split_whitespace();
        if parts.next()? != TAG_MAGIC || parts.next()? != TAG_VERSION {
            return None;
        }
        let mut pid = None;
        let mut run_id = None;
        for part in parts {
            let (key, value) = part.split_once('=')?;
            match key {
                "pid" => pid = Some(value.parse().ok()?),
                "run_id" => run_id = Some((value != NO_RUN_ID).then(|| value.to_owned())),
                _ => return None,
            }
        }
        Some(Self {
            pid: pid?,
            run_id: run_id?,
        })
    }
}

/// Metadata stored in a cached template database's comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateTag {
    /// Process id of the creator.
    pub pid: u32,
    /// Run id of the creator, if one was set.
    pub run_id: Option<String>,
    /// Creation time, seconds since the Unix epoch.
    pub created_at: u64,
    /// True only once the template initialiser finished cleanly and all sessions disconnected.
    pub ready: bool,
}

/// What a cached template database comment says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateComment {
    /// No comment: the creator died between `CREATE DATABASE` and `COMMENT ON DATABASE`.
    Missing,
    /// A template tag this version understands.
    Tag(TemplateTag),
    /// A comment this version cannot read.
    Unreadable,
}

impl TemplateTag {
    /// Render the tag as the comment text. `run_id` must already be validated.
    pub(crate) fn render(&self) -> String {
        format!(
            "{TEMPLATE_TAG_MAGIC} {TAG_VERSION} pid={} run_id={} created={} ready={}",
            self.pid,
            self.run_id.as_deref().unwrap_or(NO_RUN_ID),
            self.created_at,
            u8::from(self.ready),
        )
    }

    /// Parse a comment written by `TemplateTag::render`.
    #[must_use]
    pub fn parse(comment: Option<&str>) -> TemplateComment {
        let Some(comment) = comment else {
            return TemplateComment::Missing;
        };
        Self::parse_v1(comment).map_or(TemplateComment::Unreadable, TemplateComment::Tag)
    }

    fn parse_v1(comment: &str) -> Option<Self> {
        let mut parts = comment.split_whitespace();
        if parts.next()? != TEMPLATE_TAG_MAGIC || parts.next()? != TAG_VERSION {
            return None;
        }
        let mut pid = None;
        let mut run_id = None;
        let mut created_at = None;
        let mut ready = None;
        for part in parts {
            let (key, value) = part.split_once('=')?;
            match key {
                "pid" if pid.is_none() => pid = Some(value.parse().ok()?),
                "run_id" if run_id.is_none() => {
                    if value.is_empty() {
                        return None;
                    }
                    run_id = Some((value != NO_RUN_ID).then(|| value.to_owned()));
                }
                "created" if created_at.is_none() => created_at = Some(value.parse().ok()?),
                "ready" if ready.is_none() => {
                    ready = Some(match value {
                        "0" => false,
                        "1" => true,
                        _ => return None,
                    });
                }
                _ => return None,
            }
        }
        Some(Self {
            pid: pid?,
            run_id: run_id?,
            created_at: created_at?,
            ready: ready?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        let name = TestDbName {
            pid: 4242,
            n: 7,
            created_at: 1_760_500_000,
        };
        assert_eq!(name.render(), "erpforall_test_4242_7_1760500000");
        assert_eq!(TestDbName::parse(&name.render()), Some(name));
        assert!(is_test_db_name(&name.render()));
    }

    #[test]
    fn template_names_round_trip() {
        let digest = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ];
        let name = TemplateDbName::from_digest_prefix(digest);
        let rendered = name.render();
        assert_eq!(rendered, "erpforall_tpl_0123456789abcdeffedcba9876543210");
        assert!(rendered.len() <= 63);
        assert!(validate_template(&rendered).is_ok());
        assert_eq!(TemplateDbName::parse(&rendered), Some(name));
        assert!(is_template_db_name(&rendered));
        assert_eq!(name.lock_key(), 0x0123_4567_89ab_cdef_i64);
    }

    #[test]
    fn foreign_names_are_not_template_databases() {
        for name in [
            "erpforall",
            "erpforall_tpl_",
            "erpforall_tpl_0123",
            "erpforall_tpl_0123456789ABCDEFFEDCBA9876543210",
            "erpforall_tpl_0123456789abcdeffedcba9876543210a",
            "erpforall_tpl_0123456789abcdeffedcba987654321g",
            "erpforall_test_1_2_3",
        ] {
            assert!(!is_template_db_name(name), "{name} must not match");
        }
    }

    #[test]
    fn template_tag_round_trips() {
        let tag = TemplateTag {
            pid: 42,
            run_id: Some("ci-run-1".into()),
            created_at: 1_760_500_000,
            ready: true,
        };
        assert_eq!(
            TemplateTag::parse(Some(&tag.render())),
            TemplateComment::Tag(tag)
        );
        let pending = TemplateTag {
            pid: 7,
            run_id: None,
            created_at: 1_760_500_001,
            ready: false,
        };
        assert_eq!(
            TemplateTag::parse(Some(&pending.render())),
            TemplateComment::Tag(pending)
        );
        assert_eq!(TemplateTag::parse(None), TemplateComment::Missing);
        for bad in [
            "",
            "erpforall-testkit v1 pid=1 run_id=-",
            "erpforall-testkit-tpl v1 pid=1 run_id=- created=10 ready=2",
            "erpforall-testkit-tpl v2 pid=1 run_id=- created=10 ready=1",
            "erpforall-testkit-tpl v1 pid=1 run_id=- created=10 ready=1 extra=0",
        ] {
            assert_eq!(
                TemplateTag::parse(Some(bad)),
                TemplateComment::Unreadable,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn template_keys_are_validated() {
        assert!(validate_template_key("erp-suite-sale:sale:v1").is_ok());
        assert!(validate_template_key("").is_err());
        assert!(validate_template_key("bad\nkey").is_err());
        assert!(validate_template_key(&"k".repeat(257)).is_err());
    }

    #[test]
    fn foreign_names_are_not_test_databases() {
        for name in [
            "erpforall",
            "erpforall_test_",
            "erpforall_test_12_3",
            "erpforall_test_12_3_",
            "erpforall_test__3_4",
            "erpforall_test_1_2_3_4",
            "erpforall_test_a_1_2",
            "erpforall_test_99999999999_1_2",
            "legacy_test_1_2_3",
        ] {
            assert!(!is_test_db_name(name), "{name} must not match");
        }
    }

    #[test]
    fn tag_round_trips() {
        let tag = Tag {
            pid: 99,
            run_id: Some("ci-abc:1".into()),
        };
        assert_eq!(Tag::parse(Some(&tag.render())), Comment::Tag(tag));
        let no_run = Tag {
            pid: 1,
            run_id: None,
        };
        assert_eq!(Tag::parse(Some(&no_run.render())), Comment::Tag(no_run));
    }

    #[test]
    fn comments_are_classified() {
        assert_eq!(Tag::parse(None), Comment::Missing);
        for text in [
            "",
            "my notes",
            "erpforall-testkit v1 pid=x run_id=-",
            "erpforall-testkit v1 run_id=-",
            "erpforall-testkit v1 pid=1 run_id=- extra=1",
            "erpforall-testkit v2 pid=1 run_id=-",
        ] {
            assert_eq!(Tag::parse(Some(text)), Comment::Unreadable, "{text:?}");
        }
    }

    #[test]
    fn run_ids_are_validated() {
        assert!(validate_run_id("ci-0123abc-20261015T010101Z-77").is_ok());
        assert!(validate_run_id("").is_err());
        assert!(validate_run_id("-").is_err());
        assert!(validate_run_id("x'; DROP DATABASE erpforall; --").is_err());
        assert!(validate_run_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn templates_are_validated() {
        assert!(validate_template("template0").is_ok());
        assert!(validate_template("_bench_tpl").is_ok());
        assert!(validate_template("Template0").is_err());
        assert!(validate_template("0abc").is_err());
        assert!(validate_template("a\"b").is_err());
        assert!(validate_template("").is_err());
    }
}
