//! Cached template databases shared across test processes within a run.
//!
//! Integration suites that install the same addon graph across dozens of tests spend most of their
//! runtime repeating identical schema sync and seed-data loads. [`TemplateDb::clone_or_init`]
//! initialises a deterministic template database `erpforall_tpl_<32hex>` once per `(scope, key)`
//! on the chosen cluster, then hands each test a fresh throwaway [`TestDb`] cloned from it via
//! PostgreSQL's default `STRATEGY WAL_LOG` (`CREATE DATABASE ... TEMPLATE ...`).
//!
//! Concurrency across parallel `nextest` processes is serialised on the cluster's admin database
//! connection with `pg_advisory_lock(lock_key)` covering both template initialisation and cloning,
//! preventing PostgreSQL `55006` (`source database is being accessed by other users`). Because
//! PostgreSQL advisory locks are scoped to the connected admin database, callers and
//! [`crate::sweep`] targeting the same server must connect to the same admin database. Templates
//! carry a [`TemplateTag`] comment with `run_id`, `created` and `ready=<0|1>`, so [`crate::sweep`]
//! drops them at the end of a CI run or after their max age without interfering with active
//! builders.

use std::fmt;
use std::future::Future;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deadpool_postgres::{Pool, Runtime};
use tokio_postgres::Client;

use crate::TestDb;
use crate::cluster::{
    Chosen, Cluster, Clusters, PgProber, UsageThreshold, cluster_log_from_env, parse_url_list,
    probed_unreachable_from_env, record_choice, select, stderr_log,
};
use crate::db::{
    DEFAULT_POOL_SIZE, POOL_TIMEOUTS, connect, create_tagged, drop_database_with, quote_ident,
    quote_literal,
};
use crate::error::{Result, TestkitError, error_chain, pg_error};
use crate::tag::{
    RUN_ID_ENV, TemplateComment, TemplateDbName, TemplateTag, unix_now, validate_run_id,
    validate_template_key,
};

/// Environment variable set automatically by `cargo-nextest` per test run.
const NEXTEST_RUN_ID_ENV: &str = "NEXTEST_RUN_ID";

/// Minimum pool size used while running a template initialiser (addon installs open multiple
/// connections concurrently).
const MIN_INIT_POOL_SIZE: usize = 4;

/// Number of passive drain polls before escalating to `pg_terminate_backend` so normal async TCP
/// teardown after `Pool::close` finishes without `FATAL` termination noise.
const DRAIN_GRACE_POLLS: u32 = 5;

/// Maximum attempts when waiting for PostgreSQL client backends on a newly initialised template
/// database to exit after closing the temporary pool.
const DRAIN_ATTEMPTS: u32 = 50;

/// Pause between drain attempts when waiting for template backends to exit.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone)]
enum Pin {
    Url(String),
    Urls(String),
    Cluster(Cluster),
}

/// Entry point for creating throwaway [`TestDb`] instances cloned from a cached template database.
pub struct TemplateDb;

impl TemplateDb {
    /// Clone a fresh throwaway [`TestDb`] from the template identified by `key`, initialising the
    /// template once per test run on the selected cluster if it does not yet exist.
    ///
    /// `key` must identify the full schema and seed data produced by `init` (for example
    /// `"erp-suite-sale:sale:v1"`). Different addon sets or options must use distinct keys.
    ///
    /// # Errors
    /// Invalid key or settings, cluster selection failures, SQL failures, or any error returned by
    /// `init` (wrapped in [`TestkitError::TemplateInit`]). When `init` fails, the incomplete
    /// template database is dropped before returning so subsequent callers never observe a
    /// half-initialised template.
    pub async fn clone_or_init<F, Fut, E>(key: impl Into<String>, init: F) -> Result<TestDb>
    where
        F: FnOnce(Pool) -> Fut,
        Fut: Future<Output = std::result::Result<(), E>>,
        E: fmt::Display,
    {
        Self::builder(key).clone_or_init(init).await
    }

    /// Configure options for cloning from (and lazily initialising) the template identified by
    /// `key`.
    pub fn builder(key: impl Into<String>) -> TemplateDbBuilder {
        TemplateDbBuilder {
            key: key.into(),
            pin: None,
            max_usage: None,
            pool_size: DEFAULT_POOL_SIZE,
            run_id: None,
        }
    }
}

/// Options for [`TemplateDb::clone_or_init`]. Start with [`TemplateDb::builder`].
#[derive(Clone)]
#[must_use = "call `.clone_or_init(...).await` to clone or initialise the template"]
pub struct TemplateDbBuilder {
    key: String,
    pin: Option<Pin>,
    max_usage: Option<UsageThreshold>,
    pool_size: usize,
    run_id: Option<String>,
}

impl fmt::Debug for TemplateDbBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pin = match &self.pin {
            None => None,
            Some(Pin::Url(_)) => Some("<redacted url>".to_owned()),
            Some(Pin::Urls(_)) => Some("<redacted url list>".to_owned()),
            Some(Pin::Cluster(cluster)) => Some(cluster.to_string()),
        };
        f.debug_struct("TemplateDbBuilder")
            .field("key", &self.key)
            .field("database_url", &pin)
            .field("max_usage", &self.max_usage)
            .field("pool_size", &self.pool_size)
            .field("run_id", &self.run_id)
            .finish()
    }
}

impl TemplateDbBuilder {
    /// Pin the server instead of reading `ERP_TEST_DATABASE_URLS` / `DATABASE_URL`.
    pub fn database_url(mut self, url: impl Into<String>) -> Self {
        self.pin = Some(Pin::Url(url.into()));
        self
    }

    /// Choose among these whitespace-separated admin URLs instead of reading the environment.
    pub fn database_urls(mut self, urls: impl Into<String>) -> Self {
        self.pin = Some(Pin::Urls(urls.into()));
        self
    }

    /// Usage threshold for choosing among several clusters (ignored when pinned).
    pub fn max_usage(mut self, threshold: UsageThreshold) -> Self {
        self.max_usage = Some(threshold);
        self
    }

    /// Pin the cluster directly (for example `other.cluster().clone()`).
    pub fn cluster(mut self, cluster: Cluster) -> Self {
        self.pin = Some(Pin::Cluster(cluster));
        self
    }

    /// Maximum connections in the returned clone's pool (at least 1).
    pub fn pool_size(mut self, size: usize) -> Self {
        self.pool_size = size.max(1);
        self
    }

    /// Run id recorded in both the template tag and the cloned database tag. Defaults to
    /// `ERP_TESTKIT_RUN_ID` when set.
    pub fn run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// Compute the deterministic PostgreSQL template database name for this builder's key and
    /// effective scope without opening any connection.
    ///
    /// # Errors
    /// [`TestkitError::InvalidName`] when the key or run id is invalid.
    pub fn template_name(&self) -> Result<TemplateDbName> {
        validate_template_key(&self.key)?;
        let run_id = resolve_run_id(self.run_id.as_deref())?;
        let scope = resolve_scope(run_id.as_deref())?;
        Ok(derive_template_name(&scope, &self.key))
    }

    /// Clone a fresh [`TestDb`] from the template, initialising the template on the chosen cluster
    /// first if it is absent or incomplete.
    ///
    /// # Errors
    /// Invalid settings, cluster selection failures, SQL failures, or [`TestkitError::TemplateInit`]
    /// when `init` returns an error.
    pub async fn clone_or_init<F, Fut, E>(self, init: F) -> Result<TestDb>
    where
        F: FnOnce(Pool) -> Fut,
        Fut: Future<Output = std::result::Result<(), E>>,
        E: fmt::Display,
    {
        validate_template_key(&self.key)?;
        let run_id = resolve_run_id(self.run_id.as_deref())?;
        let scope = resolve_scope(run_id.as_deref())?;
        let tpl = derive_template_name(&scope, &self.key);
        let tpl_name = tpl.render();
        let lock_key = tpl.lock_key();

        let plan = match self.pin {
            Some(Pin::Url(url)) => Clusters::Pinned(Cluster::parse(1, 1, &url)?),
            Some(Pin::Urls(urls)) => Clusters::Ordered(parse_url_list(&urls)?),
            Some(Pin::Cluster(cluster)) => Clusters::Pinned(cluster),
            None => Clusters::from_env()?,
        };
        let log_path = cluster_log_from_env()?;

        let (cluster, admin_client, usage) = match plan {
            Clusters::Ordered(list) => {
                let threshold = match self.max_usage {
                    Some(threshold) => threshold,
                    None => UsageThreshold::from_env()?,
                };
                let reserve =
                    u32::try_from(self.pool_size.max(MIN_INIT_POOL_SIZE)).unwrap_or(u32::MAX);
                let reported = probed_unreachable_from_env()?;
                let Chosen {
                    cluster,
                    client,
                    usage,
                } = select(
                    &list,
                    threshold,
                    reserve,
                    &reported,
                    &mut PgProber,
                    &mut stderr_log,
                )
                .await?;
                (cluster, client, Some(usage))
            }
            Clusters::Pinned(cluster) => {
                let client = connect(cluster.config()).await?;
                (cluster, client, None)
            }
        };

        let now = unix_now()?;

        // Serialise template build AND `CREATE DATABASE ... TEMPLATE` on this cluster.
        admin_client
            .query_one("SELECT pg_advisory_lock($1)", &[&lock_key])
            .await
            .map_err(pg_error(format!(
                "acquiring advisory lock for template {tpl_name}"
            )))?;

        let clone_result = ensure_and_clone(
            &admin_client,
            &cluster,
            &tpl_name,
            &self.key,
            self.pool_size,
            run_id.as_deref(),
            init,
        )
        .await;

        match admin_client
            .query_one("SELECT pg_advisory_unlock($1)", &[&lock_key])
            .await
        {
            Ok(row) => {
                if row.try_get::<_, bool>(0).ok() != Some(true) {
                    eprintln!(
                        "erp-testkit: advisory lock for template {tpl_name} was not held on unlock"
                    );
                }
            }
            Err(unlock_err) => {
                eprintln!(
                    "erp-testkit: could not release advisory lock for template {tpl_name}: {}",
                    error_chain(&unlock_err)
                );
            }
        }

        let name = clone_result?;
        if let Err(error) = record_choice(
            log_path.as_deref(),
            &cluster,
            &name,
            usage,
            run_id.as_deref(),
            now,
        ) {
            if let Err(drop_error) = drop_database_with(&admin_client, &name).await {
                eprintln!(
                    "erp-testkit: could not drop {name} after the cluster log failed: {}",
                    error_chain(&drop_error)
                );
            }
            return Err(error);
        }

        let mut db_config = cluster.config().clone();
        db_config.dbname(&name);
        let pool = match erp_orm::pool::pool_builder(db_config)
            .max_size(self.pool_size)
            .timeouts(POOL_TIMEOUTS)
            .runtime(Runtime::Tokio1)
            .build()
        {
            Ok(pool) => pool,
            Err(error) => {
                if let Err(drop_error) = drop_database_with(&admin_client, &name).await {
                    eprintln!(
                        "erp-testkit: could not drop {name} after pool failure: {}",
                        error_chain(&drop_error)
                    );
                }
                return Err(error.into());
            }
        };
        // Verify connectivity and the PG18 version gate on the cloned database.
        if let Err(error) = pool.get().await {
            pool.close();
            if let Err(drop_error) = drop_database_with(&admin_client, &name).await {
                eprintln!(
                    "erp-testkit: could not drop {name} after connection failure: {}",
                    error_chain(&drop_error)
                );
            }
            return Err(error.into());
        }
        Ok(TestDb::from_parts(name, pool, cluster))
    }
}

/// Must be called while `admin_client` holds `pg_advisory_lock(lock_key)` for `tpl_name`.
async fn ensure_and_clone<F, Fut, E>(
    admin_client: &Client,
    cluster: &Cluster,
    tpl_name: &str,
    key: &str,
    pool_size: usize,
    run_id: Option<&str>,
    init: F,
) -> Result<String>
where
    F: FnOnce(Pool) -> Fut,
    Fut: Future<Output = std::result::Result<(), E>>,
    E: fmt::Display,
{
    let existing = admin_client
        .query_opt(
            "SELECT d.datdba = (SELECT oid FROM pg_roles WHERE rolname = current_user), \
                    d.datistemplate, \
                    shobj_description(d.oid, 'pg_database') \
             FROM pg_database d \
             WHERE d.datname = $1",
            &[&tpl_name],
        )
        .await
        .map_err(pg_error(format!("inspecting template database {tpl_name}")))?;

    let needs_build = match existing {
        None => true,
        Some(row) => {
            let owned: bool = row.get(0);
            let is_pg_template: bool = row.get(1);
            let comment: Option<String> = row.get(2);
            if !owned || is_pg_template {
                return Err(TestkitError::Configuration(format!(
                    "template database {tpl_name} already exists but is not an owned non-system template database"
                )));
            }
            match TemplateTag::parse(comment.as_deref()) {
                TemplateComment::Tag(tag) if tag.ready => false,
                TemplateComment::Tag(_) | TemplateComment::Missing => {
                    // Prior builder died or failed mid-init; clean up before rebuilding.
                    drop_database_with(admin_client, tpl_name).await?;
                    true
                }
                TemplateComment::Unreadable => {
                    return Err(TestkitError::Configuration(format!(
                        "template database {tpl_name} carries an unreadable comment; refusing to drop it"
                    )));
                }
            }
        }
    };

    if needs_build {
        build_template(
            admin_client,
            cluster,
            tpl_name,
            key,
            pool_size,
            run_id,
            init,
        )
        .await?;
    }

    // Still holding the advisory lock: clone the template into a fresh tagged throwaway DB.
    create_tagged(admin_client, Some(tpl_name), run_id.map(str::to_owned)).await
}

async fn build_template<F, Fut, E>(
    admin_client: &Client,
    cluster: &Cluster,
    tpl_name: &str,
    key: &str,
    pool_size: usize,
    run_id: Option<&str>,
    init: F,
) -> Result<()>
where
    F: FnOnce(Pool) -> Fut,
    Fut: Future<Output = std::result::Result<(), E>>,
    E: fmt::Display,
{
    let create_sql = format!(
        "CREATE DATABASE {} TEMPLATE template0 ENCODING 'UTF8' LC_COLLATE 'C'",
        quote_ident(tpl_name)
    );
    admin_client
        .batch_execute(&create_sql)
        .await
        .map_err(pg_error(format!("creating template database {tpl_name}")))?;

    let pid = std::process::id();
    let created_at = match unix_now() {
        Ok(ts) => ts,
        Err(err) => {
            drop_template_best_effort(admin_client, tpl_name, "clock failure").await;
            return Err(err);
        }
    };

    let init_result: Result<()> = async {
        write_template_tag(
            admin_client,
            tpl_name,
            &TemplateTag {
                pid,
                run_id: run_id.map(str::to_owned),
                created_at,
                ready: false,
            },
        )
        .await?;

        let mut tpl_config = cluster.config().clone();
        tpl_config.dbname(tpl_name);
        let init_pool = erp_orm::pool::pool_builder(tpl_config)
            .max_size(pool_size.max(MIN_INIT_POOL_SIZE))
            .timeouts(POOL_TIMEOUTS)
            .runtime(Runtime::Tokio1)
            .build()?;

        {
            let conn = init_pool.get().await?;
            conn.batch_execute("CREATE EXTENSION IF NOT EXISTS pg_trgm")
                .await
                .map_err(pg_error(format!(
                    "installing base extension pg_trgm in template {tpl_name}"
                )))?;
        }

        let user_result = init(init_pool.clone()).await;
        init_pool.close();
        drop(init_pool);

        if let Err(err) = user_result {
            return Err(TestkitError::TemplateInit {
                key: key.to_owned(),
                message: err.to_string(),
            });
        }

        // Seal the template against stray connections while keeping it cloneable (`CREATE DATABASE
        // ... TEMPLATE` clones `datallowconn = false` databases into normal `datallowconn = true`
        // clones in PostgreSQL 18).
        let seal_sql = format!(
            "ALTER DATABASE {} ALLOW_CONNECTIONS false",
            quote_ident(tpl_name)
        );
        admin_client
            .batch_execute(&seal_sql)
            .await
            .map_err(pg_error(format!(
                "disabling connections on template database {tpl_name}"
            )))?;

        drain_template_backends(admin_client, tpl_name).await?;

        write_template_tag(
            admin_client,
            tpl_name,
            &TemplateTag {
                pid,
                run_id: run_id.map(str::to_owned),
                created_at,
                ready: true,
            },
        )
        .await?;
        Ok(())
    }
    .await;

    if let Err(err) = init_result {
        drop_template_best_effort(admin_client, tpl_name, "initialisation failure").await;
        return Err(err);
    }
    Ok(())
}

async fn write_template_tag(client: &Client, tpl_name: &str, tag: &TemplateTag) -> Result<()> {
    let sql = format!(
        "COMMENT ON DATABASE {} IS {}",
        quote_ident(tpl_name),
        quote_literal(&tag.render())
    );
    client
        .batch_execute(&sql)
        .await
        .map_err(pg_error(format!("tagging template database {tpl_name}")))
}

async fn drain_template_backends(admin_client: &Client, tpl_name: &str) -> Result<()> {
    for attempt in 0..DRAIN_ATTEMPTS {
        let count: i64 = admin_client
            .query_one(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = $1 \
                   AND backend_type = 'client backend' \
                   AND pid <> pg_backend_pid()",
                &[&tpl_name],
            )
            .await
            .map_err(pg_error(format!(
                "checking active connections on template {tpl_name}"
            )))?
            .get(0);
        if count == 0 {
            return Ok(());
        }
        // Give normal async pool teardown a short grace period before terminating leaked client
        // sessions owned by the current role (never touch autovacuum or other roles' backends).
        if attempt >= DRAIN_GRACE_POLLS {
            admin_client
                .query(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = $1 \
                       AND backend_type = 'client backend' \
                       AND usename = current_user \
                       AND pid <> pg_backend_pid()",
                    &[&tpl_name],
                )
                .await
                .map_err(pg_error(format!(
                    "terminating residual connections on template {tpl_name}"
                )))?;
        }
        tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
    }
    Err(TestkitError::Configuration(format!(
        "template database {tpl_name} still had active client backends after closing its pool; \
         ensure the template initialiser drops all database connections before returning"
    )))
}

async fn drop_template_best_effort(client: &Client, tpl_name: &str, reason: &str) {
    if let Err(drop_err) = drop_database_with(client, tpl_name).await {
        eprintln!(
            "erp-testkit: could not drop template {tpl_name} after {reason}: {}",
            error_chain(&drop_err)
        );
    }
}

fn resolve_run_id(explicit: Option<&str>) -> Result<Option<String>> {
    let run_id = match explicit {
        Some(id) => Some(id.to_owned()),
        None => match std::env::var(RUN_ID_ENV) {
            Ok(id) if !id.is_empty() => Some(id),
            Ok(_) | Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(TestkitError::Configuration(format!(
                    "{RUN_ID_ENV} is not valid UTF-8"
                )));
            }
        },
    };
    if let Some(id) = &run_id {
        validate_run_id(id)?;
    }
    Ok(run_id)
}

fn process_scope() -> &'static str {
    static SCOPE: OnceLock<String> = OnceLock::new();
    SCOPE.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("pid:{}:{nanos}", std::process::id())
    })
}

fn resolve_scope(run_id: Option<&str>) -> Result<String> {
    if let Some(id) = run_id {
        return Ok(format!("run:{id}"));
    }
    match std::env::var(NEXTEST_RUN_ID_ENV) {
        Ok(nextest_id) if !nextest_id.trim().is_empty() => {
            Ok(format!("nextest:{}", nextest_id.trim()))
        }
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(process_scope().to_owned()),
        Err(std::env::VarError::NotUnicode(_)) => Err(TestkitError::Configuration(format!(
            "{NEXTEST_RUN_ID_ENV} is not valid UTF-8"
        ))),
    }
}

fn derive_template_name(scope: &str, key: &str) -> TemplateDbName {
    let mut input = Vec::with_capacity(3 + scope.len() + 1 + key.len());
    input.extend_from_slice(b"v1\0");
    input.extend_from_slice(scope.as_bytes());
    input.push(0);
    input.extend_from_slice(key.as_bytes());
    let digest = sha256(&input);
    let mut prefix = [0u8; 16];
    prefix.copy_from_slice(&digest[..16]);
    TemplateDbName::from_digest_prefix(prefix)
}

/// FIPS 180-4 SHA-256 (self-contained to keep `erp-testkit`'s dependency graph minimal).
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let offset = i * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(&mut s, "{b:02x}");
        }
        s
    }

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn template_names_are_deterministic_and_scoped() {
        let a1 = derive_template_name("run:r1", "erp-suite-sale:sale");
        let a2 = derive_template_name("run:r1", "erp-suite-sale:sale");
        let b = derive_template_name("run:r2", "erp-suite-sale:sale");
        let c = derive_template_name("run:r1", "erp-suite-sale:sale_stock");
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert_ne!(a1, c);
        assert!(crate::tag::is_template_db_name(&a1.render()));
    }

    #[test]
    fn builder_debug_redacts_url() {
        let builder = TemplateDb::builder("demo").database_url("postgres://u:secret@127.0.0.1/db");
        let text = format!("{builder:?}");
        assert!(!text.contains("secret"), "{text}");
    }
}
