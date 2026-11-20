// SPDX-License-Identifier: MIT OR Apache-2.0
//! Role bindings of addon-owned SQL objects, slice B4 (amendment A2; plan
//! ): a NOLOGIN definer, a maintenance and a
//! request login role, created for each test in the test cluster and dropped afterwards (also on
//! a failed test), checked as the roles themselves. Real PostgreSQL, one database per test.
//!
//! Fixture addon `test_sqlobj` and its model are test fixtures: they ship no default data.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use erp_fields::ErrorKind;
use erp_orm::{Orm, SUPERUSER_ID, SchemaSyncOptions, Signal, SqlRoleBindings, StaticPolicy};
use erp_registry::{
    AddonDescriptor, FieldDecl, ModelDecl, Registry, RegistryBuilder, SqlBinding, SqlDep, SqlEvent,
    SqlFunctionDecl, SqlLanguage, SqlLevel, SqlObjectDecl, SqlObjectKind, SqlObjectManifest,
    SqlResult, SqlScalar, SqlSecurity, SqlTableGrantDecl, SqlTableOp, SqlTiming, SqlTriggerDecl,
    SqlType,
};
use erp_sql::{quote, sqlobj};
use erp_test_models::base_users;
use erp_testkit::Harness;
use erp_testkit::tokio_postgres::{self, Client, NoTls, error::SqlState};

static SQLOBJ: AddonDescriptor = AddonDescriptor {
    name: "test_sqlobj",
    depends: &["base"],
    exact_only: false,
};

const ADDON: &str = "test_sqlobj";
const TOKEN: &str = "test_sqlobj.token";

fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn leak_str(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn leak_slice<T>(value: Vec<T>) -> &'static [T] {
    Box::leak(value.into_boxed_slice())
}

fn token_model() -> ModelDecl {
    ModelDecl::new(ADDON, TOKEN)
        .fields(const { &[FieldDecl::char("name"), FieldDecl::char("note")] })
}

fn function(key: &'static str, decl: SqlFunctionDecl) -> SqlObjectDecl {
    SqlObjectDecl::function(ADDON, key, decl)
}

fn int_fn(name: &'static str, body: &'static str) -> SqlFunctionDecl {
    SqlFunctionDecl::new(
        name,
        SqlLanguage::Sql,
        SqlResult::Scalar(SqlScalar::Integer),
        body,
    )
}

/// The definer entry point `sqlobj_post(text)`: it inserts into the INSERT-only token table
/// through unqualified names (`test_sqlobj_token`, `lower`) and says who it ran as.
const POST_BODY: &str = "BEGIN INSERT INTO test_sqlobj_token (name) VALUES (lower($1)); \
                         RETURN current_user::text || '/' || lower($1); END";

/// The objects/052: a definer entry point for request and maintenance, an
/// invoker function executable by `listed`, one by nobody, one by maintenance, a BEFORE INSERT
/// stamp trigger, and the token table INSERT-only for request, SELECT/INSERT for the definer.
fn objects(listed: &'static [SqlBinding]) -> Vec<SqlObjectDecl> {
    vec![
        function(
            "fn_post",
            SqlFunctionDecl::new(
                "sqlobj_post",
                SqlLanguage::PlPgSql,
                SqlResult::Scalar(SqlScalar::Text),
                POST_BODY,
            )
            .args(&[SqlType::Scalar(SqlScalar::Text)])
            .security(SqlSecurity::Definer)
            .deps(&[SqlDep::Model(TOKEN)])
            .grant_execute(&[SqlBinding::Request, SqlBinding::Maintenance]),
        ),
        function(
            "fn_listed",
            int_fn("sqlobj_listed", "SELECT 1").grant_execute(listed),
        ),
        function("fn_unlisted", int_fn("sqlobj_unlisted", "SELECT 2")),
        function(
            "fn_maint",
            int_fn("sqlobj_maint", "SELECT 3").grant_execute(&[SqlBinding::Maintenance]),
        ),
        function(
            "fn_stamp",
            SqlFunctionDecl::new(
                "sqlobj_stamp",
                SqlLanguage::PlPgSql,
                SqlResult::Trigger,
                "BEGIN NEW.note := 'stamped'; RETURN NEW; END",
            )
            .deps(&[SqlDep::Model(TOKEN)]),
        ),
        SqlObjectDecl::trigger(
            ADDON,
            "trg_stamp",
            SqlTriggerDecl::new(
                "sqlobj_stamp",
                TOKEN,
                (ADDON, "fn_stamp"),
                SqlTiming::Before,
                &[SqlEvent::Insert],
                SqlLevel::Row,
            ),
        ),
        SqlObjectDecl::table_grant(
            ADDON,
            "grant_token_request",
            SqlTableGrantDecl::new(TOKEN, SqlBinding::Request, &[SqlTableOp::Insert]),
        ),
        SqlObjectDecl::table_grant(
            ADDON,
            "grant_token_definer",
            SqlTableGrantDecl::new(
                TOKEN,
                SqlBinding::Definer,
                &[SqlTableOp::Select, SqlTableOp::Insert],
            ),
        ),
    ]
}

fn build(decls: Vec<SqlObjectDecl>) -> Arc<Registry> {
    build_model(token_model(), decls)
}

fn build_model(model: ModelDecl, decls: Vec<SqlObjectDecl>) -> Arc<Registry> {
    let decls: Vec<&'static SqlObjectDecl> = decls.into_iter().map(leak).collect();
    let keys: Vec<&'static str> = decls.iter().map(|d| d.key()).collect();
    let manifest = leak(SqlObjectManifest::new(ADDON, leak_slice(keys)));
    let model: &'static ModelDecl = leak(model);
    Arc::new(
        RegistryBuilder::new([&base_users::ADDON, &SQLOBJ])
            .with_decls([model])
            .require_sql_objects([manifest])
            .with_sql_objects(decls)
            .linked(false)
            .build()
            .unwrap(),
    )
}

/// The roles a test creates, dropped (with everything they own or were granted in the test
/// database) when this value is dropped, so also when the test fails. Declared before the
/// harness in [`Fixture`], so it drops while the database still exists.
struct Roles {
    db: tokio_postgres::Config,
    names: Vec<String>,
}

impl Roles {
    fn add(&mut self, name: String) -> String {
        self.names.push(name.clone());
        name
    }
}

impl Drop for Roles {
    fn drop(&mut self) {
        let (db, names) = (self.db.clone(), std::mem::take(&mut self.names));
        // Drop cannot await, so a thread with its own runtime does the work. The tests run on a
        // multi-thread runtime: while this blocks one worker, the other closes the connection of
        // a dropped transaction, whose locks DROP OWNED would otherwise wait for.
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async move {
                let (client, connection) = db.connect(NoTls).await.map_err(|e| e.to_string())?;
                tokio::spawn(connection);
                // a cleanup that cannot get its locks reports it instead of hanging the test
                client
                    .batch_execute("SET lock_timeout = '60s'")
                    .await
                    .map_err(|e| e.to_string())?;
                for name in names {
                    let q = quote(&name);
                    let sql = format!(
                        "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = {}) THEN \
                         REASSIGN OWNED BY {q} TO CURRENT_USER; DROP OWNED BY {q}; DROP ROLE {q}; \
                         END IF; END $$",
                        erp_sql::literal(&name)
                    );
                    client
                        .batch_execute(&sql)
                        .await
                        .map_err(|e| format!("{name}: {e}"))?;
                }
                Ok::<(), String>(())
            })
        });
        match worker.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                eprintln!("sql_objects_security: dropping test roles failed: {error}")
            }
            Err(_) => eprintln!("sql_objects_security: the role cleanup thread panicked"),
        }
    }
}

struct Fixture {
    roles: Roles,
    h: Harness,
    request: &'static str,
    maintenance: &'static str,
    definer: &'static str,
    password: String,
    owner: &'static str,
}

impl Fixture {
    /// A fresh database with `base_users` and the signal table, and a request and maintenance
    /// login role and a NOLOGIN definer, the request role named `request` after the test tag.
    async fn new(request: &str) -> Self {
        let h = Harness::builder([&base_users::ADDON])
            .build()
            .await
            .unwrap();
        let mut tx = h.begin().await.unwrap();
        tx.install_signal().await.unwrap();
        tx.commit().await.unwrap();
        let mut hasher = DefaultHasher::new();
        h.db().name().hash(&mut hasher);
        let tag = format!("sqlr{:012x}", hasher.finish() & 0xffff_ffff_ffff);
        let mut roles = Roles {
            db: h.db().config(),
            names: Vec::new(),
        };
        let request = leak_str(roles.add(format!("{tag}_{request}")));
        let maintenance = leak_str(roles.add(format!("{tag}_maint")));
        let definer = leak_str(roles.add(format!("{tag}_definer")));
        let owner = leak_str(roles.add(format!("{tag}_schema")));
        let password = format!("{tag}-login");
        let pw = erp_sql::literal(&password);
        let (r, m, d) = (quote(request), quote(maintenance), quote(definer));
        let f = Self {
            roles,
            h,
            request,
            maintenance,
            definer,
            password,
            owner,
        };
        f.admin(&format!(
            "CREATE ROLE {r} LOGIN PASSWORD {pw}; CREATE ROLE {m} LOGIN PASSWORD {pw}; \
             CREATE ROLE {d} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION \
             NOBYPASSRLS; GRANT USAGE ON SCHEMA public TO {r}, {m}"
        ))
        .await;
        let database = quote(f.h.db().name());
        let o = quote(owner);
        f.admin(&format!("CREATE ROLE {o} NOLOGIN; ALTER DATABASE {database} OWNER TO {o}; ALTER SCHEMA public OWNER TO pg_database_owner; GRANT {d} TO {o} WITH INHERIT TRUE, SET TRUE; {}", reown_public(owner))).await;
        f
    }

    fn bindings(&self) -> SqlRoleBindings {
        SqlRoleBindings::new()
            .definer(self.definer)
            .maintenance(self.maintenance)
            .request(self.request)
    }

    /// Another role of this test, dropped with the others.
    fn role(&mut self, suffix: &str) -> String {
        let name = format!("{}_{suffix}", self.definer.trim_end_matches("_definer"));
        self.roles.add(name)
    }

    /// A batch on the test database as the cluster administrator (the harness's connection).
    async fn admin(&self, sql: &str) {
        let client = self.h.db().client().await.unwrap();
        client.batch_execute(sql).await.expect(sql);
    }

    async fn admin_rows(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        let client = self.h.db().client().await.unwrap();
        let rows = client.query(sql, &[]).await.expect(sql);
        rows.iter()
            .map(|r| (0..r.len()).map(|i| r.get(i)).collect())
            .collect()
    }

    /// A connection logged in as `role`.
    async fn login(&self, role: &str) -> Client {
        let mut config = self.h.db().config();
        config.user(role).password(&self.password);
        let (client, connection) = config.connect(NoTls).await.unwrap();
        tokio::spawn(connection);
        client
    }

    /// One lifecycle schema sync of `registry` with `roles`, committed; rolled back on error.
    async fn sync_with(
        &self,
        registry: Arc<Registry>,
        roles: SqlRoleBindings,
    ) -> erp_fields::Result<()> {
        self.sync_on(None, registry, roles).await
    }

    /// [`Self::sync_with`] on connections made with `config`, else the harness pool.
    async fn sync_on(
        &self,
        config: Option<tokio_postgres::Config>,
        registry: Arc<Registry>,
        roles: SqlRoleBindings,
    ) -> erp_fields::Result<()> {
        let pool = match config {
            Some(config) => erp_orm::pool::create_pool(config, 2).unwrap(),
            None => erp_orm::pool::create_pool(self.owner_config(), 2).unwrap(),
        };
        let client = self.h.db().client().await.unwrap();
        let applied = Signal::read(&client).await.unwrap();
        drop(client);
        let orm = Orm::builder(registry, pool)
            .policy(StaticPolicy::AllowAll)
            .signal(Signal::new(applied))
            .build()
            .unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await?;
        let options = SchemaSyncOptions::inline().lifecycle(true).sql_roles(roles);
        let sync = tx.init_models_with(&[], &options).await?;
        assert!(sync.is_complete());
        tx.commit().await
    }

    async fn sync(&self, registry: Arc<Registry>) -> erp_fields::Result<()> {
        self.sync_with(registry, self.bindings()).await
    }

    /// Owner and ACL of every SQL object function and of the token table.
    async fn acls(&self) -> Vec<Vec<Option<String>>> {
        self.admin_rows(
            "SELECT p.proname::text, pg_get_userbyid(p.proowner)::text, p.proacl::text FROM \
             pg_proc p WHERE p.proname LIKE 'sqlobj\\_%' UNION ALL SELECT c.relname::text, \
             pg_get_userbyid(c.relowner)::text, c.relacl::text FROM pg_class c WHERE c.oid = \
             to_regclass('test_sqlobj_token') ORDER BY 1",
        )
        .await
    }

    fn owner_config(&self) -> tokio_postgres::Config {
        let mut config = self.h.db().config();
        config.options(format!("-c role={}", self.owner));
        config
    }

    async fn finish(self) {
        self.admin(&format!(
            "ALTER DATABASE {} OWNER TO current_user",
            quote(self.h.db().name())
        ))
        .await;
        let Self { roles, h, .. } = self;
        drop(roles);
        h.cleanup().await.unwrap();
    }
}

async fn denied(client: &Client, sql: &str) {
    let error = client.batch_execute(sql).await.expect_err(sql);
    assert_eq!(
        error.code(),
        Some(&SqlState::INSUFFICIENT_PRIVILEGE),
        "{sql}: {error:?}"
    );
}

async fn text(client: &Client, sql: &str) -> String {
    client.query_one(sql, &[]).await.expect(sql).get(0)
}

#[track_caller]
fn refused(result: erp_fields::Result<()>, kind: ErrorKind, message: &str) {
    let error = result.expect_err("the sync must refuse");
    assert_eq!(error.kind(), kind, "{error:?}");
    assert_eq!(error.message(), message);
}

/// PUBLIC executes nothing; request executes only its listed functions and the definer entry
/// point, inserts into the INSERT-only table (its identity column needs no sequence privilege,
/// the stamp trigger runs without EXECUTE on its function) and cannot read, update, delete or
/// truncate it, disable or replace a guard, read SQL object provenance or become the definer or
/// maintenance role; the entry point runs as the NOLOGIN definer, which owns it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_051_restricted_grants_are_effective() {
    let f = Fixture::new("request").await;
    f.sync(build(objects(&[SqlBinding::Request])))
        .await
        .unwrap();
    let owner = Some(f.owner.to_owned());
    let catalog = f
        .admin_rows(
            "SELECT p.proname::text, pg_get_userbyid(p.proowner)::text, EXISTS (SELECT FROM \
             aclexplode(coalesce(p.proacl, acldefault('f', p.proowner))) a WHERE a.grantee = 0)::text \
             FROM pg_proc p WHERE p.proname LIKE 'sqlobj\\_%' ORDER BY 1",
        )
        .await;
    let expected: Vec<Vec<Option<String>>> = [
        ("sqlobj_listed", owner.clone()),
        ("sqlobj_maint", owner.clone()),
        ("sqlobj_post", Some(f.definer.to_owned())),
        ("sqlobj_stamp", owner.clone()),
        ("sqlobj_unlisted", owner.clone()),
    ]
    .into_iter()
    .map(|(name, owner)| vec![Some(name.to_owned()), owner, Some("false".to_owned())])
    .collect();
    assert_eq!(catalog, expected);

    let request = f.login(f.request).await;
    assert_eq!(
        text(&request, "SELECT public.sqlobj_listed()::text").await,
        "1"
    );
    denied(&request, "SELECT public.sqlobj_unlisted()").await;
    denied(&request, "SELECT public.sqlobj_maint()").await;
    request
        .batch_execute("INSERT INTO test_sqlobj_token (name) VALUES ('direct')")
        .await
        .unwrap();
    for sql in [
        "SELECT name FROM test_sqlobj_token",
        "UPDATE test_sqlobj_token SET name = name",
        "DELETE FROM test_sqlobj_token",
        "TRUNCATE test_sqlobj_token",
        "ALTER TABLE test_sqlobj_token DISABLE TRIGGER sqlobj_stamp",
        "CREATE OR REPLACE FUNCTION public.sqlobj_listed() RETURNS integer LANGUAGE sql AS \
         'SELECT 9'",
        "DROP FUNCTION public.sqlobj_unlisted()",
        "SELECT addon FROM erp_schema_sql_object",
        "SELECT addon FROM erp_schema_sql_object_dependency",
    ] {
        denied(&request, sql).await;
    }
    denied(
        &request,
        &format!(
            "ALTER FUNCTION public.sqlobj_post(text) OWNER TO {}",
            quote(f.request)
        ),
    )
    .await;
    denied(&request, &format!("SET ROLE {}", quote(f.definer))).await;
    denied(&request, &format!("SET ROLE {}", quote(f.maintenance))).await;
    assert_eq!(
        text(&request, "SELECT public.sqlobj_post('ABC')").await,
        format!("{}/abc", f.definer)
    );

    let maintenance = f.login(f.maintenance).await;
    assert_eq!(
        text(&maintenance, "SELECT public.sqlobj_maint()::text").await,
        "3"
    );
    assert_eq!(
        text(&maintenance, "SELECT public.sqlobj_post('M')").await,
        format!("{}/m", f.definer)
    );
    denied(&maintenance, "SELECT public.sqlobj_listed()").await;
    denied(
        &maintenance,
        "INSERT INTO test_sqlobj_token (name) VALUES ('m2')",
    )
    .await;
    assert_eq!(
        f.admin_rows("SELECT name, note FROM test_sqlobj_token ORDER BY name")
            .await,
        [["abc", "stamped"], ["direct", "stamped"], ["m", "stamped"]]
            .map(|r| r.map(|c| Some(c.to_owned())).to_vec())
            .to_vec()
    );
    drop((request, maintenance));
    f.finish().await;
}

/// A privilege the definer holds beyond its policy is found after the objects, tables and grants
/// were made in the transaction: the sync fails Access and nothing of it remains.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_051_a_late_grant_failure_rolls_everything_back() {
    let f = Fixture::new("request").await;
    f.admin(&format!(
        "CREATE TABLE sqlr_other (x int); GRANT SELECT ON sqlr_other TO {}",
        quote(f.definer)
    ))
    .await;
    refused(
        f.sync(build(objects(&[SqlBinding::Request]))).await,
        ErrorKind::Access,
        &format!(
            "SQL object role binding 'definer': role {:?} has forbidden SELECT on table \
             \"sqlr_other\"",
            f.definer
        ),
    );
    assert_eq!(
        f.admin_rows(
            "SELECT (SELECT count(*) FROM pg_proc WHERE proname LIKE 'sqlobj\\_%')::text, \
             (to_regclass('test_sqlobj_token') IS NULL)::text"
        )
        .await,
        vec![vec![Some("0".to_owned()), Some("true".to_owned())]]
    );
    f.finish().await;
}

/// A request session that plants `lower(text)` in a schema ahead of `pg_catalog` on its path and
/// a temporary table named like the token table still has the definer entry point use
/// `pg_catalog.lower` and the persistent table: the function's search path is core-rendered
/// with `pg_temp` last (A1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_052_definer_functions_resist_shadowing() {
    let f = Fixture::new("request").await;
    f.sync(build(objects(&[SqlBinding::Request])))
        .await
        .unwrap();
    let r = quote(f.request);
    f.admin(&format!(
        "CREATE SCHEMA sqlr_plant; GRANT USAGE, CREATE ON SCHEMA sqlr_plant TO {r}"
    ))
    .await;
    let request = f.login(f.request).await;
    request
        .batch_execute(
            "CREATE FUNCTION sqlr_plant.lower(text) RETURNS text LANGUAGE sql AS \
             $$SELECT 'planted'$$; \
             CREATE TEMP TABLE test_sqlobj_token (name text, note text); \
             SET search_path = sqlr_plant, pg_catalog, public; \
             INSERT INTO test_sqlobj_token (name) VALUES (lower('SELF'))",
        )
        .await
        .unwrap();
    // the plants are live on the caller's own path
    assert_eq!(
        text(
            &request,
            "SELECT string_agg(name, ',') FROM test_sqlobj_token"
        )
        .await,
        "planted"
    );
    assert_eq!(
        text(&request, "SELECT public.sqlobj_post('ABC')").await,
        format!("{}/abc", f.definer)
    );
    assert_eq!(
        text(
            &request,
            "SELECT count(*)::text FROM pg_temp.test_sqlobj_token"
        )
        .await,
        "1"
    );
    assert_eq!(
        f.admin_rows("SELECT name FROM public.test_sqlobj_token")
            .await,
        vec![vec![Some("abc".to_owned())]]
    );
    drop(request);
    f.finish().await;
}

/// install is the ROLES-PUBLISH follow-up)
/// A privilege granted by hand to a role the request role reaches (INHERIT, or SET only) on the
/// restricted table or an unlisted function fails Access naming object, role, privilege and the
/// holder; a broad table grant to the request role itself and PUBLIC grants (also column grants)
/// are revoked by the reconciliation, which leaves exactly the declared policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_052_effective_privileges_through_membership() {
    let mut f = Fixture::new("request").await;
    let registry = build(objects(&[SqlBinding::Request]));
    f.sync(Arc::clone(&registry)).await.unwrap();
    let clean = f.acls().await;
    let helper = f.role("helper");
    let (r, x) = (quote(f.request), quote(&helper));
    let leak = |object: &str, what: &str| {
        format!(
            "SQL object 'test_sqlobj/{object}': role {:?} has forbidden {what} (held by role \
             {helper:?}, which it reaches through membership)",
            f.request
        )
    };
    let table = leak(
        "grant_token_request",
        "SELECT on table \"test_sqlobj_token\"",
    );
    f.admin(&format!(
        "CREATE ROLE {x} NOLOGIN; GRANT SELECT ON test_sqlobj_token TO {x}; GRANT {x} TO {r}"
    ))
    .await;
    refused(
        f.sync(Arc::clone(&registry)).await,
        ErrorKind::Access,
        &table,
    );
    f.admin(&format!(
        "REVOKE {x} FROM {r}; GRANT {x} TO {r} WITH INHERIT FALSE, SET TRUE"
    ))
    .await;
    refused(
        f.sync(Arc::clone(&registry)).await,
        ErrorKind::Access,
        &table,
    );
    f.admin(&format!(
        "REVOKE SELECT ON test_sqlobj_token FROM {x}; \
         GRANT EXECUTE ON FUNCTION public.sqlobj_unlisted() TO {x}"
    ))
    .await;
    let before = f.acls().await;
    refused(
        f.sync(Arc::clone(&registry)).await,
        ErrorKind::Access,
        &leak(
            "fn_unlisted",
            "EXECUTE on function \"public\".\"sqlobj_unlisted\"()",
        ),
    );
    assert_eq!(f.acls().await, before, "a refused sync changes no grant");

    // what the reconciliation itself takes away
    f.admin(&format!(
        "REVOKE EXECUTE ON FUNCTION public.sqlobj_unlisted() FROM {x}; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {r}; \
         GRANT EXECUTE ON FUNCTION public.sqlobj_unlisted() TO PUBLIC; \
         GRANT SELECT (name) ON test_sqlobj_token TO PUBLIC"
    ))
    .await;
    f.sync(Arc::clone(&registry)).await.unwrap();
    assert_eq!(f.acls().await, clean);
    let request = f.login(f.request).await;
    denied(&request, "SELECT public.sqlobj_unlisted()").await;
    denied(&request, "SELECT name FROM test_sqlobj_token").await;
    denied(&request, "UPDATE test_sqlobj_token SET note = note").await;
    request
        .batch_execute("INSERT INTO test_sqlobj_token (name) VALUES ('ok')")
        .await
        .unwrap();
    drop(request);
    f.finish().await;
}

/// An object naming an unset binding fails Programming; a missing, LOGIN, BYPASSRLS, CREATEROLE
/// or superuser-reaching role, two bindings on one role, and bound roles that reach each other
/// fail Access, all before any DDL; reserved and invalid role names are refused by the shared
/// grantee checks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_052_unsafe_bindings_fail_before_any_ddl() {
    let mut f = Fixture::new("request").await;
    let registry = build(objects(&[SqlBinding::Request]));
    let (r, m, d) = (quote(f.request), quote(f.maintenance), quote(f.definer));
    let su = f.role("su");
    let s = quote(&su);
    let nothing_installed = || async {
        assert_eq!(
            f.admin_rows(
                "SELECT (SELECT count(*) FROM pg_proc WHERE proname LIKE 'sqlobj\\_%')::text"
            )
            .await,
            vec![vec![Some("0".to_owned())]]
        );
    };
    refused(
        f.sync_with(
            Arc::clone(&registry),
            SqlRoleBindings::new()
                .maintenance(f.maintenance)
                .request(f.request),
        )
        .await,
        ErrorKind::Programming,
        "SQL object 'test_sqlobj/fn_post': role binding 'definer' is not configured; the \
         operator names its database role in the schema sync role bindings",
    );
    // restricted objects are proven against the request role, so it is needed even when no
    // object names it
    refused(
        f.sync_with(
            build(vec![function(
                "fn_def",
                int_fn("sqlobj_def", "SELECT 1")
                    .security(SqlSecurity::Definer)
                    .grant_execute(&[SqlBinding::Maintenance]),
            )]),
            SqlRoleBindings::new()
                .definer(f.definer)
                .maintenance(f.maintenance),
        )
        .await,
        ErrorKind::Programming,
        "SQL object 'test_sqlobj/fn_def': role binding 'request' is not configured; the \
         operator names its database role in the schema sync role bindings",
    );
    let hint = "; the definer is a role the operator creates NOLOGIN NOSUPERUSER NOCREATEDB \
                NOCREATEROLE NOREPLICATION NOBYPASSRLS and grants to the schema owner WITH \
                INHERIT TRUE, SET TRUE";
    let absent = leak_str(format!("{}_absent", f.definer));
    refused(
        f.sync_with(Arc::clone(&registry), f.bindings().definer(absent))
            .await,
        ErrorKind::Access,
        &format!("SQL object role binding 'definer': role {absent:?} does not exist{hint}"),
    );
    refused(
        f.sync_with(Arc::clone(&registry), f.bindings().maintenance(f.request))
            .await,
        ErrorKind::Access,
        &format!(
            "SQL object role bindings 'maintenance' and 'request' name the same role {:?}; each \
             binding needs a role of its own",
            f.request
        ),
    );
    let attribute_cases = [
        (
            format!("ALTER ROLE {d} LOGIN"),
            format!("ALTER ROLE {d} NOLOGIN"),
            format!(
                "SQL object role binding 'definer': role {:?} can log in; the definer must be a \
                 NOLOGIN role{hint}",
                f.definer
            ),
        ),
        (
            format!("ALTER ROLE {m} BYPASSRLS"),
            format!("ALTER ROLE {m} NOBYPASSRLS"),
            format!(
                "SQL object role binding 'maintenance': role {:?} has BYPASSRLS; no SQL object \
                 role binding may have it",
                f.maintenance
            ),
        ),
        (
            format!("ALTER ROLE {r} CREATEROLE"),
            format!("ALTER ROLE {r} NOCREATEROLE"),
            format!(
                "SQL object role binding 'request': role {:?} has CREATEROLE: it can give itself \
                 the membership of other roles whatever this grants, so the SQL object request \
                 role must not have it",
                f.request
            ),
        ),
        (
            format!("CREATE ROLE {s} SUPERUSER NOLOGIN; GRANT {s} TO {r}"),
            format!("REVOKE {s} FROM {r}"),
            format!(
                "SQL object role binding 'request': role {:?} is a member of {su:?}, a superuser \
                 role: it can act as a superuser whatever this grants, so the SQL object request \
                 role must not reach it",
                f.request
            ),
        ),
        (
            format!("GRANT {d} TO {r}"),
            format!("REVOKE {d} FROM {r}"),
            format!(
                "SQL object role binding 'request': role {:?} is a member of role {:?} of \
                 binding 'definer'; the bound roles must not reach each other",
                f.request, f.definer
            ),
        ),
        (
            format!("GRANT {m} TO {r} WITH INHERIT FALSE, SET FALSE"),
            format!("REVOKE {m} FROM {r}"),
            format!(
                "SQL object role binding 'request': role {:?} is a member of role {:?} of \
                 binding 'maintenance'; the bound roles must not reach each other",
                f.request, f.maintenance
            ),
        ),
    ];
    for (setup, undo, message) in attribute_cases {
        f.admin(&setup).await;
        refused(
            f.sync(Arc::clone(&registry)).await,
            ErrorKind::Access,
            &message,
        );
        f.admin(&undo).await;
        nothing_installed().await;
    }
    // hostile names never reach a statement: reserved, NUL, overlong
    let long = leak_str("r".repeat(64));
    for name in ["public", "PUBLIC", "none", "pg_monitor", "a\0b", long] {
        let error = f
            .sync_with(Arc::clone(&registry), f.bindings().request(name))
            .await
            .expect_err(name);
        assert_eq!(error.kind(), ErrorKind::Access, "{name:?}: {error:?}");
        assert!(
            error
                .message()
                .starts_with("SQL object role binding 'request': "),
            "{name:?}: {error:?}"
        );
    }
    nothing_installed().await;
    // all bindings safe: the same sync installs
    f.sync(registry).await.unwrap();
    f.finish().await;
}

/// A request role whose name holds a quote, a semicolon and a comment marker is bound, granted
/// and logged in as like any other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_051_hostile_role_names_are_quoted() {
    let f = Fixture::new("q\"x; DROP TABLE res_users; --").await;
    f.sync(build(objects(&[SqlBinding::Request])))
        .await
        .unwrap();
    let request = f.login(f.request).await;
    assert_eq!(
        text(&request, "SELECT public.sqlobj_listed()::text").await,
        "1"
    );
    denied(&request, "SELECT public.sqlobj_unlisted()").await;
    assert_eq!(
        f.admin_rows("SELECT (to_regclass('res_users') IS NOT NULL)::text")
            .await,
        vec![vec![Some("true".to_owned())]]
    );
    drop(request);
    f.finish().await;
}

/// A second sync leaves owners and ACLs as they were; a binding the declaration no longer lists
/// loses its EXECUTE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_051_reconciliation_is_idempotent_and_revokes_removed_grants() {
    let f = Fixture::new("request").await;
    f.sync(build(objects(&[SqlBinding::Request])))
        .await
        .unwrap();
    let first = f.acls().await;
    f.sync(build(objects(&[SqlBinding::Request])))
        .await
        .unwrap();
    assert_eq!(f.acls().await, first);
    let request = f.login(f.request).await;
    assert_eq!(
        text(&request, "SELECT public.sqlobj_listed()::text").await,
        "1"
    );
    f.sync(build(objects(&[]))).await.unwrap();
    denied(&request, "SELECT public.sqlobj_listed()").await;
    drop(request);
    f.finish().await;
}

/// non-superuser schema owner)
/// `ALTER FUNCTION … OWNER TO` the definer needs the schema owner's SET membership in the
/// definer and the definer's CREATE on the schema, which [`sqlobj::transfer_to_definer`] gives for
/// the one statement and takes back; `CREATE OR REPLACE` of the definer-owned function needs the
/// INHERIT membership.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_052_definer_transfer_needs_membership_and_schema_create() {
    let mut f = Fixture::new("request").await;
    let registry = build(vec![function(
        "fn_probe",
        int_fn("sqlr_probe", "SELECT 1").security(SqlSecurity::Definer),
    )]);
    let SqlObjectKind::Function(probe) = registry
        .sql_objects()
        .get(ADDON, "fn_probe")
        .unwrap()
        .kind()
    else {
        panic!("fn_probe is a function");
    };
    let owner = f.role("owner");
    let (o, d) = (quote(&owner), quote(f.definer));
    let database = quote(f.h.db().name());
    let client = f.h.db().client().await.unwrap();
    let create = "CREATE FUNCTION public.sqlr_probe() RETURNS integer LANGUAGE sql SECURITY \
                  DEFINER AS 'SELECT 1'";
    client
        .batch_execute(&format!(
            "CREATE ROLE {o} NOLOGIN; ALTER DATABASE {database} OWNER TO {o}; \
             ALTER SCHEMA public OWNER TO pg_database_owner; SET ROLE {o}; {create}; RESET ROLE"
        ))
        .await
        .unwrap();
    let as_owner = |sql: String| {
        let client = &client;
        let o = o.clone();
        async move {
            let result = client.batch_execute(&format!("SET ROLE {o}; {sql}")).await;
            client.batch_execute("RESET ROLE").await.unwrap();
            result
        }
    };
    let transfer = sqlobj::transfer_to_definer(probe, &d);
    let plain = format!("ALTER FUNCTION public.sqlr_probe() OWNER TO {d}");
    let replace = "CREATE OR REPLACE FUNCTION public.sqlr_probe() RETURNS integer LANGUAGE sql \
                   SECURITY DEFINER AS 'SELECT 2'"
        .to_owned();
    let code = |r: Result<(), tokio_postgres::Error>| r.expect_err("must fail").code().cloned();
    // no membership: cannot hand the function over
    assert_eq!(
        code(as_owner(plain.clone()).await),
        Some(SqlState::INSUFFICIENT_PRIVILEGE)
    );
    // SET membership without INHERIT: the definer still needs CREATE on the schema
    client
        .batch_execute(&format!("GRANT {d} TO {o} WITH INHERIT FALSE, SET TRUE"))
        .await
        .unwrap();
    assert_eq!(
        code(as_owner(plain).await),
        Some(SqlState::INSUFFICIENT_PRIVILEGE)
    );
    as_owner(transfer).await.unwrap();
    let state = "SELECT pg_get_userbyid(proowner)::text FROM pg_proc WHERE proname = \
                 'sqlr_probe'";
    assert_eq!(text(&client, state).await, f.definer);
    assert_eq!(
        text(
            &client,
            &format!(
                "SELECT has_schema_privilege({}, 'public', 'CREATE')::text",
                erp_sql::literal(f.definer)
            )
        )
        .await,
        "false"
    );
    // replacing the definer-owned body needs the INHERIT membership
    assert_eq!(
        code(as_owner(replace.clone()).await),
        Some(SqlState::INSUFFICIENT_PRIVILEGE)
    );
    client
        .batch_execute(&format!(
            "REVOKE {d} FROM {o}; GRANT {d} TO {o} WITH INHERIT TRUE, SET TRUE"
        ))
        .await
        .unwrap();
    as_owner(replace).await.unwrap();
    assert_eq!(text(&client, state).await, f.definer);
    assert_eq!(text(&client, "SELECT public.sqlr_probe()::text").await, "2");
    let admin = text(&client, "SELECT session_user::text").await;
    client
        .batch_execute(&format!(
            "ALTER DATABASE {database} OWNER TO {}",
            quote(&admin)
        ))
        .await
        .unwrap();
    drop(client);
    f.finish().await;
}

/// column grant held by a reached role, and grants to PUBLIC the reconciliation cannot revoke)
/// Each excess fails Access after the reconciliation, naming the object, the bound role, the
/// privilege and where it comes from; the sync then commits nothing of itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_052_schema_column_and_public_leaks_are_refused() {
    let mut f = Fixture::new("request").await;
    let registry = build(objects(&[SqlBinding::Request]));
    f.sync(Arc::clone(&registry)).await.unwrap();
    let clean = f.acls().await;
    let helper = f.role("helper");
    let (r, x) = (quote(f.request), quote(&helper));
    f.admin(&format!("CREATE ROLE {x} NOLOGIN")).await;
    let cases = [
        (
            format!("GRANT CREATE ON SCHEMA public TO {r}"),
            format!("REVOKE CREATE ON SCHEMA public FROM {r}"),
            format!(
                "SQL object role binding 'request': role {:?} has forbidden CREATE on schema \
                 \"public\"",
                f.request
            ),
        ),
        (
            format!("GRANT SELECT (note) ON test_sqlobj_token TO {x}; GRANT {x} TO {r}"),
            format!("REVOKE {x} FROM {r}; REVOKE SELECT (note) ON test_sqlobj_token FROM {x}"),
            format!(
                "SQL object 'test_sqlobj/grant_token_request': role {:?} has forbidden SELECT on \
                 table \"test_sqlobj_token\" (held by role {helper:?}, which it reaches through \
                 membership)",
                f.request
            ),
        ),
        // granted to PUBLIC by another grantor: the owner's REVOKE does not reach it
        (
            format!(
                "GRANT SELECT ON test_sqlobj_token TO {x} WITH GRANT OPTION; SET ROLE {x}; \
                 GRANT SELECT ON test_sqlobj_token TO PUBLIC; RESET ROLE"
            ),
            format!("REVOKE SELECT ON test_sqlobj_token FROM {x} CASCADE"),
            // PUBLIC has no policy of its own: the table's first policy names it
            "SQL object 'test_sqlobj/grant_token_definer': PUBLIC has forbidden SELECT on table \
             \"test_sqlobj_token\""
                .to_owned(),
        ),
        // another relation PUBLIC may read: the definer reaches it through PUBLIC
        (
            "CREATE TABLE sqlr_pub (x int); GRANT SELECT ON sqlr_pub TO PUBLIC".to_owned(),
            "DROP TABLE sqlr_pub".to_owned(),
            format!(
                "SQL object role binding 'definer': role {:?} has forbidden SELECT on table \
                 \"sqlr_pub\" (granted to PUBLIC)",
                f.definer
            ),
        ),
    ];
    for (setup, undo, message) in cases {
        f.admin(&setup).await;
        let before = f.acls().await;
        refused(
            f.sync(Arc::clone(&registry)).await,
            ErrorKind::Access,
            &message,
        );
        assert_eq!(
            f.acls().await,
            before,
            "{message}: a refused sync changes no grant"
        );
        f.admin(&undo).await;
    }
    f.sync(registry).await.unwrap();
    assert_eq!(f.acls().await, clean);
    f.finish().await;
}

/// OWNER)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_051_a_hostile_definer_name_is_quoted() {
    let mut f = Fixture::new("request").await;
    let definer = leak_str(f.role("d\"x; DROP TABLE res_users; --"));
    f.admin(&format!(
        "CREATE ROLE {} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
        quote(definer)
    ))
    .await;
    f.admin(&format!(
        "GRANT {} TO {} WITH INHERIT TRUE, SET TRUE",
        quote(definer),
        quote(f.owner)
    ))
    .await;
    f.sync_with(
        build(objects(&[SqlBinding::Request])),
        f.bindings().definer(definer),
    )
    .await
    .unwrap();
    assert_eq!(
        f.admin_rows(
            "SELECT pg_get_userbyid(proowner)::text, (to_regclass('res_users') IS NOT NULL)::text \
             FROM pg_proc WHERE proname = 'sqlobj_post'"
        )
        .await,
        vec![vec![Some(definer.to_owned()), Some("true".to_owned())]]
    );
    let request = f.login(f.request).await;
    assert_eq!(
        text(&request, "SELECT public.sqlobj_post('Q')").await,
        format!("{definer}/q")
    );
    drop(request);
    f.finish().await;
}

/// Every relation, sequence and routine of `public` the test administrator owns, given to
/// `owner`, so `owner` can run schema sync on the database the harness built.
fn reown_public(owner: &str) -> String {
    let owner = erp_sql::literal(owner);
    format!(
        "DO $$ DECLARE r record; me oid := (SELECT oid FROM pg_roles WHERE rolname = current_user);
         BEGIN
           FOR r IN SELECT c.oid::regclass AS t, c.relkind FROM pg_class c
                     WHERE c.relnamespace = 'public'::regnamespace AND c.relowner = me
                       AND c.relkind IN ('r', 'p', 'v', 'm', 'f') LOOP
             EXECUTE format('ALTER %s %s OWNER TO %I', CASE r.relkind WHEN 'v' THEN 'VIEW'
               WHEN 'm' THEN 'MATERIALIZED VIEW' WHEN 'f' THEN 'FOREIGN TABLE' ELSE 'TABLE' END,
               r.t, {owner});
           END LOOP;
           FOR r IN SELECT c.oid::regclass AS t FROM pg_class c
                     WHERE c.relnamespace = 'public'::regnamespace AND c.relowner = me
                       AND c.relkind = 'S' LOOP
             EXECUTE format('ALTER SEQUENCE %s OWNER TO %I', r.t, {owner});
           END LOOP;
           FOR r IN SELECT p.oid::regprocedure AS f FROM pg_proc p
                     WHERE p.pronamespace = 'public'::regnamespace AND p.proowner = me LOOP
             EXECUTE format('ALTER ROUTINE %s OWNER TO %I', r.f, {owner});
           END LOOP;
         END $$"
    )
}

/// Run as a schema owner that is not a superuser, the sync refuses by name while the owner lacks
/// the definer membership; with it, the sync hands the definer function over (CREATE on the
/// schema for that statement only), reconciles its grants through the INHERIT membership, and a
/// second sync leaves every owner and ACL as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_ormx_052_a_non_superuser_owner_syncs_through_its_definer_membership() {
    let mut f = Fixture::new("request").await;
    let owner = f.role("owner");
    let (o, d) = (quote(&owner), quote(f.definer));
    let database = quote(f.h.db().name());
    f.admin(&format!(
        "CREATE ROLE {o} NOLOGIN; ALTER DATABASE {database} OWNER TO {o}; \
         ALTER SCHEMA public OWNER TO pg_database_owner; {}",
        reown_public(&owner).replace(
            "rolname = current_user",
            &format!("rolname = {}", erp_sql::literal(f.owner))
        )
    ))
    .await;
    let mut config = f.h.db().config();
    // the harness login, acting as the owner for the whole session (role names here are plain)
    config.options(format!("-c role={owner}"));
    let registry = build(objects(&[SqlBinding::Request]));
    refused(
        f.sync_on(Some(config.clone()), Arc::clone(&registry), f.bindings())
            .await,
        ErrorKind::Access,
        &format!(
            "SQL object role binding 'definer': the schema owner {owner:?} cannot act as role \
             {:?}; the operator runs GRANT {d} TO {o} WITH INHERIT TRUE, SET TRUE",
            f.definer
        ),
    );
    f.admin(&format!("GRANT {d} TO {o} WITH INHERIT TRUE, SET TRUE"))
        .await;
    f.sync_on(Some(config.clone()), Arc::clone(&registry), f.bindings())
        .await
        .unwrap();
    let first = f.acls().await;
    let owners: Vec<(Option<String>, Option<String>)> = first
        .iter()
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect();
    let mine = |name: &str, who: &str| (Some(name.to_owned()), Some(who.to_owned()));
    assert_eq!(
        owners,
        vec![
            mine("sqlobj_listed", &owner),
            mine("sqlobj_maint", &owner),
            mine("sqlobj_post", f.definer),
            mine("sqlobj_stamp", &owner),
            mine("sqlobj_unlisted", &owner),
            mine("test_sqlobj_token", &owner),
        ]
    );
    assert_eq!(
        f.admin_rows(&format!(
            "SELECT has_schema_privilege({}, 'public', 'CREATE')::text",
            erp_sql::literal(f.definer)
        ))
        .await,
        vec![vec![Some("false".to_owned())]]
    );
    f.sync_on(Some(config), Arc::clone(&registry), f.bindings())
        .await
        .unwrap();
    assert_eq!(f.acls().await, first);
    let request = f.login(f.request).await;
    assert_eq!(
        text(&request, "SELECT public.sqlobj_post('ABC')").await,
        format!("{}/abc", f.definer)
    );
    denied(&request, "SELECT public.sqlobj_unlisted()").await;
    drop(request);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b6_unlogged_table_preserves_insert_only_policy() {
    use erp_registry::{SqlTableOptionsDecl, SqlTablePersistence};
    let f = Fixture::new("b6_request").await;
    let declarations = |persistence| {
        vec![
            SqlObjectDecl::table_options(
                ADDON,
                "table",
                SqlTableOptionsDecl::new(TOKEN, persistence),
            ),
            SqlObjectDecl::table_grant(
                ADDON,
                "grant",
                SqlTableGrantDecl::new(TOKEN, SqlBinding::Request, &[SqlTableOp::Insert]),
            ),
        ]
    };
    let model = || {
        ModelDecl::new(ADDON, TOKEN)
            .fields(
                const {
                    &[
                        FieldDecl::char("name"),
                        FieldDecl::char("note"),
                        FieldDecl::boolean("enabled")
                            .required(true)
                            .default(erp_registry::StaticValue::Bool(true))
                            .db_default(true),
                    ]
                },
            )
            .table_objects(
                const {
                    &[erp_registry::TableObjectDecl {
                        name: "_safe_name",
                        kind: erp_registry::TableObjectKind::Constraint {
                            definition: "CHECK (name <> 'bad')",
                            message: "This token name is refused.",
                        },
                    }]
                },
            )
    };
    let r = build_model(model(), declarations(SqlTablePersistence::Unlogged));
    f.sync(r.clone()).await.unwrap();
    assert_eq!(
        f.admin_rows(
            "SELECT relpersistence::text FROM pg_class WHERE oid='test_sqlobj_token'::regclass"
        )
        .await,
        vec![vec![Some("u".to_owned())]]
    );
    let request = f.login(f.request).await;
    request
        .batch_execute("INSERT INTO test_sqlobj_token(name) VALUES('kept')")
        .await
        .unwrap();
    for sql in [
        "SELECT * FROM test_sqlobj_token",
        "UPDATE test_sqlobj_token SET name='changed'",
        "DELETE FROM test_sqlobj_token",
        "TRUNCATE test_sqlobj_token",
    ] {
        denied(&request, sql).await;
    }
    let orm = Orm::builder(r.clone(), f.h.db().pool().clone())
        .policy(StaticPolicy::AllowAll)
        .build()
        .unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let e = tx
        .execute(
            "INSERT INTO test_sqlobj_token(name,note) VALUES('bad','SENSITIVE_ROW_VALUE')",
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Validation);
    assert_eq!(e.sqlstate(), Some("23514"));
    assert_eq!(
        e.message(),
        "The database refused the change: This token name is refused."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    f.sync(r).await.unwrap();
    assert_eq!(
        f.admin_rows("SELECT enabled::text FROM test_sqlobj_token")
            .await,
        vec![vec![Some("true".to_owned())]]
    );
    assert_eq!(
        f.admin_rows("SELECT name FROM test_sqlobj_token").await,
        vec![vec![Some("kept".to_owned())]]
    );
    refused(
        f.sync(build_model(
            model(),
            declarations(SqlTablePersistence::Logged),
        ))
        .await,
        ErrorKind::Programming,
        "SQL object 'test_sqlobj/table': table persistence changed; explicit migration required",
    );
    drop(request);
    f.finish().await;
}

#[tokio::test]
async fn deferred_objects_and_table_policies_receive_grants_when_their_table_is_ready() {
    let f = Fixture::new("deferred").await;
    let registry = build(objects(&[SqlBinding::Request]));
    let users = registry.model("res.users").unwrap().id;
    let token = registry.model(TOKEN).unwrap().id;
    let orm = Orm::builder(
        registry,
        erp_orm::pool::create_pool(f.owner_config(), 2).unwrap(),
    )
    .policy(StaticPolicy::AllowAll)
    .build()
    .unwrap();
    let options = SchemaSyncOptions::inline()
        .lifecycle(true)
        .sql_roles(f.bindings());
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    assert!(
        tx.init_models_with(&[users], &options)
            .await
            .unwrap()
            .is_complete()
    );
    assert!(
        tx.query("SELECT to_regclass('test_sqlobj_token')::text", &[])
            .await
            .unwrap()[0]
            .get::<_, Option<String>>(0)
            .is_none()
    );
    assert_eq!(
        tx.query(
            "SELECT count(*) FROM erp_schema_sql_object WHERE key='fn_post'",
            &[]
        )
        .await
        .unwrap()[0]
            .get::<_, i64>(0),
        0
    );
    assert!(
        tx.init_models_with(&[token], &options)
            .await
            .unwrap()
            .is_complete()
    );
    tx.refuse_deferred_sql_objects(&options).await.unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    let request = f.login(f.request).await;
    assert_eq!(text(&request, "SELECT sqlobj_listed()::text").await, "1");
    let posted = text(&request, "SELECT sqlobj_post('DEFERRED')").await;
    assert_eq!(posted, format!("{}/deferred", f.definer));
    denied(&request, "UPDATE test_sqlobj_token SET name='bad'").await;
    drop(request);
    f.finish().await;
}

#[tokio::test]
async fn deferral_does_not_hide_a_missing_role_binding() {
    let f = Fixture::new("missing_deferred").await;
    let registry = build(vec![function(
        "fn_def",
        int_fn("sqlobj_def", "SELECT 1")
            .security(SqlSecurity::Definer)
            .deps(&[SqlDep::Model(TOKEN)]),
    )]);
    let users = registry.model("res.users").unwrap().id;
    let orm = Orm::builder(
        registry,
        erp_orm::pool::create_pool(f.owner_config(), 2).unwrap(),
    )
    .policy(StaticPolicy::AllowAll)
    .build()
    .unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.capture_statements(true);
    let error = tx
        .init_models_with(
            &[users],
            &SchemaSyncOptions::inline()
                .lifecycle(true)
                .sql_roles(SqlRoleBindings::new().request(f.request)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming);
    assert_eq!(
        error.message(),
        "SQL object 'test_sqlobj/fn_def': role binding 'definer' is not configured; the operator names its database role in the schema sync role bindings"
    );
    assert!(
        !tx.captured()
            .iter()
            .any(|s| s.trim_start().starts_with("CREATE "))
    );
    tx.rollback().await.unwrap();
    drop(tx);
    f.finish().await;
}

#[tokio::test]
async fn schema_owner_superuser_is_refused_before_mutation() {
    let f = Fixture::new("owner_refusal").await;
    let registry = build(objects(&[SqlBinding::Request]));
    let error = f
        .sync_on(Some(f.h.db().config()), registry, f.bindings())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    assert!(
        error
            .message()
            .contains("SQL object role binding 'schema_owner'"),
        "{error}"
    );
    assert!(error.message().contains("forbidden SUPERUSER"), "{error}");
    for (flag, clear) in [
        ("CREATEROLE", "NOCREATEROLE"),
        ("REPLICATION", "NOREPLICATION"),
        ("BYPASSRLS", "NOBYPASSRLS"),
    ] {
        f.admin(&format!("ALTER ROLE {} {flag}", quote(f.owner)))
            .await;
        let error = f
            .sync(build(objects(&[SqlBinding::Request])))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access);
        assert!(
            error.message().contains(&format!("forbidden {flag}")),
            "{error}"
        );
        f.admin(&format!("ALTER ROLE {} {clear}", quote(f.owner)))
            .await;
    }

    assert_eq!(
        f.admin_rows("SELECT (to_regclass('test_sqlobj_token') IS NULL)::text")
            .await[0][0]
            .as_deref(),
        Some("true")
    );
    f.finish().await;
}

#[tokio::test]
async fn grants_only_publication_restores_policy_and_final_check_detects_widening() {
    let f = Fixture::new("publisher").await;
    let registry = build(objects(&[SqlBinding::Request]));
    f.sync(Arc::clone(&registry)).await.unwrap();
    let orm = Orm::builder(
        Arc::clone(&registry),
        erp_orm::pool::create_pool(f.owner_config(), 2).unwrap(),
    )
    .policy(StaticPolicy::AllowAll)
    .build()
    .unwrap();
    let options = SchemaSyncOptions::new().sql_roles(f.bindings());
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    tx.reconcile_sql_object_grants(&registry, &options)
        .await
        .unwrap();
    tx.verify_sql_object_publication(&registry, &options)
        .await
        .unwrap();
    tx.batch(&format!(
        "GRANT SELECT ON test_sqlobj_token TO {}",
        quote(f.request)
    ))
    .await
    .unwrap();
    let error = tx
        .verify_sql_object_publication(&registry, &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    assert!(error.message().contains("forbidden SELECT"), "{error}");
    tx.rollback().await.unwrap();
    drop(tx);
    let request = f.login(f.request).await;
    denied(&request, "SELECT * FROM test_sqlobj_token").await;
    drop(request);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_named_id_sequence_is_usable_only_by_the_configured_definer() {
    let mut f = Fixture::new("named_sequence").await;
    let registry = exclusive_registry();
    f.sync(Arc::clone(&registry)).await.unwrap();
    let admin = f.h.db().client().await.unwrap();
    let check = async || {
        let row = admin.query_one(
            "SELECT has_sequence_privilege($1::text::name,'sqlobj_token_ids','USAGE'),
                    has_sequence_privilege($1::text::name,'sqlobj_token_ids','SELECT,UPDATE'),
                    has_sequence_privilege($2::text::name,'sqlobj_token_ids','USAGE,SELECT,UPDATE'),
                    has_sequence_privilege($3::text::name,'sqlobj_token_ids','USAGE,SELECT,UPDATE')",
            &[&f.definer, &f.request, &f.maintenance],
        ).await.unwrap();
        assert!(row.get::<_, bool>(0));
        for i in 1..4 {
            assert!(!row.get::<_, bool>(i));
        }
    };
    check().await;
    admin
        .batch_execute(&format!(
            "SET ROLE {}; INSERT INTO test_sqlobj_token(name) VALUES ('admitted'); RESET ROLE",
            quote(f.definer)
        ))
        .await
        .unwrap();
    let request = f.login(f.request).await;
    denied(
        &request,
        "INSERT INTO test_sqlobj_token(id, name) VALUES (121, 'private')",
    )
    .await;
    let refused = request
        .query_one("SELECT nextval('public.sqlobj_token_ids')", &[])
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
    f.admin(&format!(
        "GRANT USAGE, SELECT ON SEQUENCE sqlobj_token_ids TO {}",
        quote(f.request)
    ))
    .await;
    f.sync(Arc::clone(&registry)).await.unwrap();
    check().await;
    f.admin("CREATE TABLE sqlobj_unrelated(id bigint DEFAULT nextval('sqlobj_token_ids'))")
        .await;
    let err = f.sync(Arc::clone(&registry)).await.unwrap_err();
    assert!(err.message().contains("sqlobj_token_ids"), "{err:?}");
    f.admin("DROP TABLE sqlobj_unrelated").await;
    f.admin("ALTER SEQUENCE sqlobj_token_ids CACHE 4").await;
    let cached = f.sync(Arc::clone(&registry)).await.unwrap_err();
    assert_eq!(cached.kind(), ErrorKind::Programming);
    assert!(cached.message().contains("sqlobj_token_ids"), "{cached}");
    f.admin("ALTER SEQUENCE sqlobj_token_ids CACHE 1").await;
    f.admin("ALTER SEQUENCE sqlobj_token_ids SET UNLOGGED")
        .await;
    let unlogged = f.sync(Arc::clone(&registry)).await.unwrap_err();
    assert_eq!(unlogged.kind(), ErrorKind::Programming);
    assert!(
        unlogged.message().contains("sqlobj_token_ids"),
        "{unlogged}"
    );
    f.admin("ALTER SEQUENCE sqlobj_token_ids SET LOGGED").await;

    let admission = Orm::builder(
        Arc::clone(&registry),
        erp_orm::pool::create_pool(f.owner_config(), 2).unwrap(),
    )
    .policy(StaticPolicy::AllowAll)
    .build()
    .unwrap();
    let options = SchemaSyncOptions::new().sql_roles(f.bindings());
    let mut startup = admission.begin(SUPERUSER_ID).await.unwrap();
    startup
        .verify_sql_object_publication(&registry, &options)
        .await
        .unwrap();
    startup
        .batch("GRANT SELECT ON SEQUENCE sqlobj_token_ids TO PUBLIC")
        .await
        .unwrap();
    let error = startup
        .verify_sql_object_publication(&registry, &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    assert!(error.message().contains("sqlobj_token_ids"), "{error}");
    startup.rollback().await.unwrap();
    drop(startup);
    drop(admission);
    check().await;

    // A same-name replacement with the same model default and schema owner is still a distinct
    // sequence. Preserve the old object and restore it after this isolated drift check.
    f.admin(&format!("SET ROLE {}; ALTER SEQUENCE sqlobj_token_ids RENAME TO sqlobj_old_ids; \
        CREATE SEQUENCE sqlobj_token_ids; ALTER TABLE test_sqlobj_token ALTER COLUMN id SET DEFAULT nextval('sqlobj_token_ids'); RESET ROLE",
        quote(f.owner))).await;
    let changed = f.sync(Arc::clone(&registry)).await.unwrap_err();
    assert!(
        changed.message().contains("identity or owner differs"),
        "{changed}"
    );
    f.admin("ALTER TABLE test_sqlobj_token ALTER COLUMN id SET DEFAULT nextval('sqlobj_old_ids'); DROP SEQUENCE sqlobj_token_ids; ALTER SEQUENCE sqlobj_old_ids RENAME TO sqlobj_token_ids").await;
    check().await;

    // A foreign grant through a role survives normal role-specific REVOKE. Admission
    // must refuse it, rolling back the new publisher's own ACL changes.
    let group = f.roles.add(format!("{}_extra", f.request));
    f.admin(&format!(
        "CREATE ROLE {} NOLOGIN; GRANT {} TO {}; GRANT SELECT ON SEQUENCE sqlobj_token_ids TO {}",
        quote(&group),
        quote(&group),
        quote(f.request),
        quote(&group)
    ))
    .await;
    f.admin(&format!(
        "GRANT UPDATE ON SEQUENCE sqlobj_token_ids TO {}",
        quote(f.request)
    ))
    .await;
    assert!(f.sync(Arc::clone(&registry)).await.is_err());
    let preserved = admin
        .query_one(
            "SELECT has_sequence_privilege($1::text::name,'sqlobj_token_ids','UPDATE')",
            &[&f.request],
        )
        .await
        .unwrap();
    assert!(
        preserved.get::<_, bool>(0),
        "failed reconciliation must restore the prior ACL"
    );
    f.admin(&format!(
        "REVOKE UPDATE ON SEQUENCE sqlobj_token_ids FROM {}",
        quote(f.request)
    ))
    .await;
    f.admin(&format!(
        "REVOKE SELECT ON SEQUENCE sqlobj_token_ids FROM {}",
        quote(&group)
    ))
    .await;

    let next = f.roles.add(format!("{}_next", f.definer));
    f.admin(&format!(
        "CREATE ROLE {} NOLOGIN; GRANT USAGE ON SCHEMA public TO {}; GRANT {} TO {}",
        quote(&next),
        quote(&next),
        quote(&next),
        quote(f.owner)
    ))
    .await;
    let previous_group = f.roles.add(format!("{}_old_group", f.definer));
    f.admin(&format!(
        "CREATE ROLE {group} NOLOGIN; GRANT SELECT ON test_sqlobj_token TO {group}; GRANT {group} TO {old} WITH INHERIT FALSE, SET TRUE",
        group = quote(&previous_group), old = quote(f.definer),
    )).await;
    let retained = f
        .sync_with(
            Arc::clone(&registry),
            f.bindings().definer_owned(next.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(retained.kind(), ErrorKind::Access);
    assert!(retained.message().contains("retains access"), "{retained}");
    f.admin(&format!(
        "REVOKE SELECT ON test_sqlobj_token FROM {}",
        quote(&previous_group)
    ))
    .await;
    f.sync_with(
        Arc::clone(&registry),
        f.bindings().definer_owned(next.clone()),
    )
    .await
    .unwrap();
    let rotated = admin.query_one("SELECT has_sequence_privilege($1::text::name,'sqlobj_token_ids','USAGE,SELECT,UPDATE'),has_table_privilege($1::text::name,'test_sqlobj_token','INSERT'),has_sequence_privilege($2::text::name,'sqlobj_token_ids','USAGE')",&[&f.definer,&next]).await.unwrap();
    assert!(!rotated.get::<_, bool>(0));
    assert!(!rotated.get::<_, bool>(1));
    assert!(rotated.get::<_, bool>(2));

    let removed = build_model(token_model().id_sequence("sqlobj_token_ids"), vec![]);
    let error = f.sync(removed).await.unwrap_err();
    assert!(
        error
            .message()
            .contains("recorded in SQL object provenance"),
        "{error:?}"
    );
    let empty = RegistryBuilder::new([&base_users::ADDON])
        .linked(false)
        .build()
        .unwrap();
    let options = SchemaSyncOptions::new().sql_roles(f.bindings().definer_owned(next));
    let orm = Orm::builder(
        registry,
        erp_orm::pool::create_pool(f.owner_config(), 2).unwrap(),
    )
    .policy(StaticPolicy::AllowAll)
    .build()
    .unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    for reconcile in [false, true] {
        let err = if reconcile {
            tx.reconcile_sql_object_grants(&empty, &options).await
        } else {
            tx.verify_sql_object_publication(&empty, &options).await
        }
        .unwrap_err();
        assert!(err.message().contains("exclusive sequence"), "{err}");
    }
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    drop(request);
    drop(admin);
    f.finish().await;
}

fn exclusive_registry() -> Arc<Registry> {
    let key = "token_writer";
    build_model(
        token_model().id_sequence("sqlobj_token_ids"),
        vec![SqlObjectDecl::table_grant(
            ADDON,
            key,
            SqlTableGrantDecl::new(TOKEN, SqlBinding::Definer, &[SqlTableOp::Insert])
                .exclusive_id_sequence("sqlobj_token_ids"),
        )],
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopting_an_exclusive_id_policy_migrates_only_the_affected_database() {
    let f = Fixture::new("upgrade_seq").await;
    f.sync(build_model(
        token_model().id_sequence("sqlobj_token_ids"),
        vec![],
    ))
    .await
    .unwrap();
    f.admin(&format!(
        "GRANT USAGE, SELECT ON SEQUENCE sqlobj_token_ids TO {}",
        quote(f.request)
    ))
    .await;
    let before = f.admin_rows(
        "SELECT count(*)::text FROM pg_attribute WHERE attrelid=to_regclass('erp_schema_sql_object') AND attname='sequence_oid'"
    ).await;
    assert_eq!(before[0][0].as_deref(), Some("0"));
    f.sync(exclusive_registry()).await.unwrap();
    let admin = f.h.db().client().await.unwrap();
    let upgraded = admin
        .query_one(
            "SELECT pg_typeof(sequence_oid)::text, pg_typeof(sequence_owner_oid)::text,
                has_sequence_privilege($1::text::name,sequence_oid,'USAGE,SELECT,UPDATE'),
                has_sequence_privilege($2::text::name,sequence_oid,'USAGE')
         FROM erp_schema_sql_object WHERE addon=$3 AND key='token_writer'",
            &[&f.request, &f.definer, &ADDON],
        )
        .await
        .unwrap();
    assert_eq!(upgraded.get::<_, String>(0), "regclass");
    assert_eq!(upgraded.get::<_, String>(1), "regrole");
    assert!(!upgraded.get::<_, bool>(2));
    assert!(upgraded.get::<_, bool>(3));
    drop(admin);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_sequence_survives_a_logical_database_restore() {
    let f = Fixture::new("restore_seq").await;
    let registry = exclusive_registry();
    f.sync(Arc::clone(&registry)).await.unwrap();
    let source = f.h.db();
    let clone = erp_testkit::TestDb::builder()
        .cluster(source.cluster().clone())
        .create()
        .await
        .unwrap();
    let admin = clone.client().await.unwrap();
    admin
        .batch_execute(&format!(
            "ALTER DATABASE {} OWNER TO {}",
            quote(clone.name()),
            quote(f.owner)
        ))
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("fixture.dump");
    for (program, db, flags) in [
        ("pg_dump", source, &["--format=custom", "--file"][..]),
        ("pg_restore", &clone, &["--exit-on-error"][..]),
    ] {
        let config = db.config();
        let bin = std::env::var_os("ERP_PG_BIN")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                [
                    "/opt/homebrew/opt/postgresql@18/bin",
                    "/usr/lib/postgresql/18/bin",
                ]
                .iter()
                .map(std::path::PathBuf::from)
                .find(|path| path.join(program).is_file())
            })
            .expect("PostgreSQL 18 client programs required");
        let tokio_postgres::config::Host::Tcp(host) = &config.get_hosts()[0] else {
            panic!("fixture requires a TCP test cluster")
        };
        let result = std::process::Command::new(bin.join(program))
            .env(
                "PGPASSWORD",
                std::str::from_utf8(config.get_password().unwrap()).unwrap(),
            )
            .args([
                "--host",
                host,
                "--port",
                &config.get_ports()[0].to_string(),
                "--username",
                config.get_user().unwrap(),
                "--dbname",
                db.name(),
            ])
            .args(flags)
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let seq_oid = "SELECT 'public.sqlobj_token_ids'::regclass::oid";
    let original: u32 = source
        .client()
        .await
        .unwrap()
        .query_one(seq_oid, &[])
        .await
        .unwrap()
        .get(0);
    let restored: u32 = admin.query_one(seq_oid, &[]).await.unwrap().get(0);
    assert_ne!(original, restored);
    let mut config = clone.config();
    config.options(format!("-c role={}", f.owner));
    let orm = Orm::builder(
        Arc::clone(&registry),
        erp_orm::pool::create_pool(config, 2).unwrap(),
    )
    .policy(StaticPolicy::AllowAll)
    .build()
    .unwrap();
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let options = SchemaSyncOptions::new().sql_roles(f.bindings());
    tx.verify_sql_object_publication(&registry, &options)
        .await
        .unwrap();
    tx.reconcile_sql_object_grants(&registry, &options)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drop(tx);
    drop(orm);
    drop(admin);
    clone.cleanup().await.unwrap();
    f.finish().await;
}
