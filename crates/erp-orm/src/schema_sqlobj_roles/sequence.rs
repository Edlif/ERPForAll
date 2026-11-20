// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exact sequence ownership, former definer cleanup and effective privilege admission.
use super::{BoundRoles, decode};
use crate::Transaction;
use erp_fields::{Error, Result};
use erp_registry::{SqlBinding, SqlObjectGraph, SqlObjectId, SqlObjectKind};
use std::collections::BTreeSet;

// Only the id default of the declared model may depend on an exclusive sequence.
// PostgreSQL catalogue dependencies are checked in addition to the composed model graph.
const TARGET: &str = "
SELECT s.oid, s.relowner, def.oid,
       s.relowner = current_user::regrole::oid AND t.relowner=s.relowner
       AND s.relpersistence='p'
       AND ps.seqtypid='bigint'::regtype AND ps.seqincrement=1 AND ps.seqcache=1 AND ps.seqmin=1 AND ps.seqmax=9223372036854775807 AND NOT ps.seqcycle
       AND pg_get_expr(ad.adbin,ad.adrelid)=format('nextval(%L::regclass)',s.oid::regclass::text)
       AND EXISTS (SELECT 1 FROM pg_depend d WHERE d.refclassid='pg_class'::regclass AND d.refobjid=s.oid AND d.classid='pg_attrdef'::regclass AND d.objid=ad.oid)
       AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.refclassid='pg_class'::regclass AND d.refobjid=s.oid AND (d.classid<>'pg_attrdef'::regclass OR d.objid<>ad.oid))
       AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid='pg_class'::regclass AND d.objid=s.oid AND d.deptype IN ('a','i','e'))
FROM pg_namespace ns JOIN pg_class s ON s.relnamespace=ns.oid AND s.relkind='S'
JOIN pg_sequence ps ON ps.seqrelid=s.oid
JOIN pg_class t ON t.relnamespace=ns.oid AND t.relname=$2 AND t.relkind IN ('r','p')
JOIN pg_attribute a ON a.attrelid=t.oid AND a.attname='id' AND NOT a.attisdropped
JOIN pg_attrdef ad ON ad.adrelid=t.oid AND ad.adnum=a.attnum
JOIN pg_roles def ON def.rolname=$3
WHERE ns.nspname='public' AND s.relname=$1";

const DIRECT_LEAK: &str = "
SELECT coalesce(r.rolname::text,'PUBLIC'), acl.privilege_type
FROM pg_class c CROSS JOIN LATERAL aclexplode(coalesce(c.relacl,acldefault('S',c.relowner))) acl
LEFT JOIN pg_roles r ON r.oid=acl.grantee
WHERE c.oid=$1 AND acl.grantee<>c.relowner
AND (acl.grantee<>$2 OR acl.privilege_type<>'USAGE' OR acl.is_grantable)
ORDER BY 1,2 LIMIT 1";

#[derive(Clone, Copy)]
struct Authority {
    sequence: u32,
    owner: u32,
    definer: u32,
}

struct Policy<'a> {
    id: SqlObjectId,
    table: &'a str,
    sequence: &'static str,
}

fn policies<'a>(
    graph: &'a SqlObjectGraph,
    deferred: &'a BTreeSet<SqlObjectId>,
) -> impl Iterator<Item = Policy<'a>> {
    graph
        .objects()
        .iter()
        .filter(|object| !deferred.contains(&object.id()))
        .filter_map(|object| {
            let SqlObjectKind::TableGrant(grant) = object.kind() else {
                return None;
            };
            Some(Policy {
                id: object.id(),
                table: grant.model().table(),
                sequence: grant.exclusive_id_sequence()?,
            })
        })
}

const RECORD: &str = "
UPDATE erp_schema_sql_object p SET sequence_oid=s.oid,
       sequence_owner_oid=s.relowner, sequence_grantee_oid=r.oid
FROM pg_class s JOIN pg_namespace n ON n.oid=s.relnamespace CROSS JOIN pg_roles r
WHERE n.nspname='public' AND s.relname=$3 AND s.relkind='S' AND r.rolname=$4
  AND p.addon=$1 AND p.key=$2 AND p.kind='exclusive_sequence'
  AND (p.sequence_oid IS NULL OR (p.sequence_oid=s.oid
       AND p.sequence_owner_oid=s.relowner AND p.sequence_grantee_oid=r.oid))";

const RETAINED_TABLE_ACCESS: &str = "
SELECT EXISTS (
    SELECT 1 FROM pg_roles role
    WHERE (role.oid=$1::oid OR pg_has_role($1::oid, role.oid, 'SET'))
      AND (has_table_privilege(role.oid, $2::text::regclass, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
           OR has_any_column_privilege(role.oid, $2::text::regclass, 'SELECT,INSERT,UPDATE,REFERENCES')))";

impl Transaction {
    /// Persist only the catalogue identity checked during this installation transaction.
    pub(crate) async fn record_named_sequences(
        &mut self,
        graph: &SqlObjectGraph,
        bound: &BoundRoles,
        deferred: &BTreeSet<SqlObjectId>,
    ) -> Result<()> {
        for Policy { id, sequence, .. } in policies(graph, deferred) {
            let definer = &bound.get(SqlBinding::Definer)?.role;
            if self
                .execute(RECORD, &[&id.addon(), &id.key(), &sequence, definer])
                .await?
                != 1
            {
                return Err(Error::programming(format!(
                    "SQL object '{id}': cannot record exclusive sequence binding for '{sequence}'"
                )));
            }
        }
        Ok(())
    }

    pub(super) async fn check_named_sequences(
        &mut self,
        graph: &SqlObjectGraph,
        bound: &BoundRoles,
        deferred: &BTreeSet<SqlObjectId>,
        apply: bool,
    ) -> Result<()> {
        for policy in policies(graph, deferred) {
            let definer = bound.get(SqlBinding::Definer)?;
            let current = self
                .named_sequence_authority(&policy, &definer.role)
                .await?;
            let previous = self.previous_sequence_definer(&policy, &current).await?;
            let changed = previous.filter(|role| *role != current.definer);
            if apply {
                self.reconcile_sequence(&policy, bound, &current, changed)
                    .await?;
            } else if changed.is_some() {
                return Err(Error::access(format!(
                    "SQL object '{}': exclusive sequence '{}' is bound to a different definer; reconcile its grants",
                    policy.id, policy.sequence
                )));
            }
            self.verify_sequence_acl(&policy, bound, &current).await?;
        }
        Ok(())
    }

    async fn named_sequence_authority(
        &mut self,
        policy: &Policy<'_>,
        definer: &str,
    ) -> Result<Authority> {
        let rows = self
            .query(TARGET, &[&policy.sequence, &policy.table, &definer])
            .await?;
        let row = rows.first().ok_or_else(|| {
            Error::programming(format!(
                "SQL object '{}': exclusive sequence '{}' or its owning id default is missing",
                policy.id, policy.sequence
            ))
        })?;
        if !row.try_get::<_, bool>(3).map_err(decode)? {
            return Err(Error::programming(format!(
                "SQL object '{}': exclusive sequence '{}' does not have its required owner, parameters and single id default",
                policy.id, policy.sequence
            )));
        }
        Ok(Authority {
            sequence: row.try_get(0).map_err(decode)?,
            owner: row.try_get(1).map_err(decode)?,
            definer: row.try_get(2).map_err(decode)?,
        })
    }

    async fn previous_sequence_definer(
        &mut self,
        policy: &Policy<'_>,
        current: &Authority,
    ) -> Result<Option<u32>> {
        let stored = self
            .query(
                "SELECT sequence_oid::oid, sequence_owner_oid::oid, sequence_grantee_oid::oid
             FROM erp_schema_sql_object WHERE addon=$1 AND key=$2",
                &[&policy.id.addon(), &policy.id.key()],
            )
            .await?;
        let Some(row) = stored.first() else {
            return Ok(None);
        };
        let previous: Option<u32> = row.try_get(2).map_err(decode)?;
        if row.try_get::<_, Option<u32>>(0).map_err(decode)? != Some(current.sequence)
            || row.try_get::<_, Option<u32>>(1).map_err(decode)? != Some(current.owner)
            || previous.is_none()
        {
            return Err(Error::programming(format!(
                "SQL object '{}': exclusive sequence '{}' identity or owner differs from its installed binding",
                policy.id, policy.sequence
            )));
        }
        Ok(previous)
    }

    async fn reconcile_sequence(
        &mut self,
        policy: &Policy<'_>,
        bound: &BoundRoles,
        current: &Authority,
        former: Option<u32>,
    ) -> Result<()> {
        let qualified = format!("public.{}", erp_sql::quote(policy.sequence));
        let mut roles: BTreeSet<_> = bound.roles.iter().map(|b| b.quoted.clone()).collect();
        if let Some(old) = former {
            let rows = self
                .query("SELECT rolname::text FROM pg_roles WHERE oid=$1", &[&old])
                .await?;
            if let Some(row) = rows.first() {
                let name: String = row.try_get(0).map_err(decode)?;
                if !bound.roles.iter().any(|b| b.role == name) {
                    self.revoke_previous_table_access(policy, old, &name)
                        .await?;
                }
                roles.insert(erp_sql::quote(&name));
            }
        }
        let mut sql = format!("REVOKE ALL PRIVILEGES ON SEQUENCE {qualified} FROM PUBLIC");
        for role in roles {
            sql.push_str(&format!(
                "; REVOKE ALL PRIVILEGES ON SEQUENCE {qualified} FROM {role}"
            ));
        }
        sql.push_str(&format!(
            "; GRANT USAGE ON SEQUENCE {qualified} TO {}",
            bound.get(SqlBinding::Definer)?.quoted
        ));
        self.sql_object_ddl(policy.id, &sql).await?;
        if former.is_some() {
            self.execute("UPDATE erp_schema_sql_object SET sequence_grantee_oid=$3::oid WHERE addon=$1 AND key=$2",
                &[&policy.id.addon(), &policy.id.key(), &current.definer]).await?;
        }
        Ok(())
    }

    async fn revoke_previous_table_access(
        &mut self,
        policy: &Policy<'_>,
        old: u32,
        name: &str,
    ) -> Result<()> {
        let table = format!("public.{}", erp_sql::quote(policy.table));
        let previous = erp_sql::quote(name);
        self.sql_object_ddl(
            policy.id,
            &format!("REVOKE ALL PRIVILEGES ON TABLE {table} FROM {previous}"),
        )
        .await?;
        let rows = self.query(RETAINED_TABLE_ACCESS, &[&old, &table]).await?;
        let row = rows
            .first()
            .ok_or_else(|| Error::programming("missing former definer privilege result"))?;
        if row.try_get::<_, bool>(0).map_err(decode)? {
            return Err(Error::access(format!(
                "SQL object '{}': former definer {name:?} retains access to {table}",
                policy.id
            )));
        }
        Ok(())
    }

    async fn verify_sequence_acl(
        &mut self,
        policy: &Policy<'_>,
        bound: &BoundRoles,
        current: &Authority,
    ) -> Result<()> {
        let qualified = format!("public.{}", erp_sql::quote(policy.sequence));
        let rows = self
            .query(DIRECT_LEAK, &[&current.sequence, &current.definer])
            .await?;
        if let Some(row) = rows.first() {
            let holder: String = row.try_get(0).map_err(decode)?;
            let privilege: String = row.try_get(1).map_err(decode)?;
            return Err(Error::access(format!(
                "SQL object '{}': role {holder:?} has unexpected {privilege} on sequence {qualified}",
                policy.id
            )));
        }
        for b in &bound.roles {
            let rows = self.query(
                "SELECT has_sequence_privilege($1::text::name,$2::oid,'USAGE'),
                        has_sequence_privilege($1::text::name,$2::oid,'SELECT,UPDATE,USAGE WITH GRANT OPTION')",
                &[&b.role, &current.sequence],
            ).await?;
            let row = rows
                .first()
                .ok_or_else(|| Error::programming("missing sequence privilege result"))?;
            let usage: bool = row.try_get(0).map_err(decode)?;
            let other: bool = row.try_get(1).map_err(decode)?;
            if usage != (b.binding == SqlBinding::Definer) || other {
                return Err(Error::access(format!(
                    "SQL object '{}': effective sequence privileges for role {:?} differ on {qualified}",
                    policy.id, b.role
                )));
            }
        }
        Ok(())
    }
}
