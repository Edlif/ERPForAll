// SPDX-License-Identifier: MIT OR Apache-2.0
//! Current-binary upgrade of the relevant stock 1.0.0 shape at main revision
//! 1c93ac1c64ae137b5e7275b3b9bd47731bec5b34: no stock security data or SQL objects,
//! nullable reservation days and legacy locations without policies. Unrelated current metadata
//! and prerequisite addons remain installed; this is not a previous-binary rehearsal.

use super::*;
use erp_testkit::tokio_postgres::Client;

const GROUPS: &[&str] = &[
    "group_stock_user",
    "group_stock_manager",
    "group_stock_multi_locations",
    "group_stock_multi_warehouses",
    "group_stock_picking_batch",
    "group_production_lot",
    "group_stock_lot_print_gs1",
    "group_lot_on_delivery_slip",
    "group_tracking_lot",
    "group_adv_location",
    "group_tracking_owner",
    "group_warning_stock",
    "group_stock_sign_delivery",
];

// The shared error envelope is already covered by the ORM's database-error tests. Read its
// private constant from our source rather than duplicate that compatibility wording here;
// the stock policy text below remains an independent oracle for the selected constraint.
fn constraint_message(policy: &str) -> String {
    let source = include_str!("../../../erp-orm/src/dberror.rs");
    let literal = source
        .lines()
        .find_map(|line| line.strip_prefix("pub(crate) const PREFIX: &str = "))
        .expect("the ORM declares its database-error envelope")
        .strip_suffix(';')
        .expect("the envelope declaration ends in a semicolon");
    let prefix: String = serde_json::from_str(literal).unwrap();
    format!("{prefix}{policy}")
}

fn stock_manifest_version() -> String {
    let manifest: toml::Table = toml::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/stock/manifest.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    manifest["version"].as_str().unwrap().to_owned()
}

async fn image(client: &Client, sql: &str) -> Vec<String> {
    client
        .query(sql, &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn policies(client: &Client) -> Vec<String> {
    image(
        client,
        "SELECT row_to_json(p)::text FROM stock_location_policy p ORDER BY id",
    )
    .await
}

async fn identities(client: &Client) -> Vec<String> {
    image(
        client,
        "SELECT json_build_array(id,name,model,res_id,noupdate)::text
         FROM ir_model_data WHERE module='stock' ORDER BY name",
    )
    .await
}

// Compare upgrade with fresh-install security semantics, independent of newly allocated IDs
// and reload audit fields. The explicit GROUPS list also detects missing fresh-install groups.
async fn security_contents(client: &Client) -> Vec<String> {
    image(
        client,
        "SELECT json_build_array('group',d.name,g.name,g.sequence,p.name)::text
         FROM ir_model_data d JOIN res_groups g ON g.id=d.res_id AND d.model='res.groups'
         LEFT JOIN res_groups_privilege p ON p.id=g.privilege_id WHERE d.module='stock'
         UNION ALL
         SELECT json_build_array('access',d.name,a.name,m.model,gd.module,gd.name,a.operation,a.domain)::text
         FROM ir_model_data d JOIN ir_access a ON a.id=d.res_id AND d.model='ir.access'
         JOIN ir_model m ON m.id=a.model_id
         LEFT JOIN ir_model_data gd ON gd.model='res.groups' AND gd.res_id=a.group_id
         WHERE d.module='stock' ORDER BY 1",
    )
    .await
}

async fn stable_state(client: &Client) -> Vec<Vec<String>> {
    vec![
        policies(client).await,
        identities(client).await,
        security_contents(client).await,
        image(client, "SELECT row_to_json(t)::text FROM stock_picking_type t ORDER BY id").await,
        image(client, "SELECT json_build_array(gid,hid)::text FROM res_groups_implied_rel ORDER BY gid,hid").await,
        image(client, "SELECT row_to_json(p)::text FROM stock_picking p ORDER BY id").await,
        image(client, "SELECT json_build_array(key,physical_identity,shape,encode(declaration_sha256,'hex'),encode(installed_sha256,'hex'))::text FROM erp_schema_sql_object WHERE addon='stock' ORDER BY key").await,
    ]
}

async fn operation_types(orm: &Orm) -> Vec<i64> {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let location: i64 = tx.query("SELECT res_id FROM ir_model_data WHERE module='stock' AND name='stock_location_suppliers'", &[]).await.unwrap()[0].get(0);
    let mut ids = Vec::new();
    for name in ["Legacy null days", "Legacy configured days"] {
        ids.push(
            tx.env()
                .records("stock.picking.type")
                .unwrap()
                .create(
                    &mut tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", Cell::Str(name.into()))
                            .set_cell("company_id", Cell::Id(company))
                            .set_cell("default_location_src_id", Cell::Id(location))
                            .set_cell("default_location_dest_id", Cell::Id(location)),
                    ],
                )
                .await
                .unwrap()
                .ids()[0],
        );
    }
    tx.commit().await.unwrap();
    ids
}

async fn restore_legacy_shape(client: &Client, operations: &[i64]) -> Vec<i64> {
    // These objects are all absent from the pinned main. Remove the physical objects as well
    // as their provenance, so schema reconciliation must install them, rather than admit drift.
    client.batch_execute("BEGIN;
        DROP TRIGGER stock_move_done_at_guard ON stock_move;
        DROP TRIGGER stock_line_done_at_guard ON stock_move_line;
        DROP TRIGGER stock_correction_update_guard ON stock_move_line_correction;
        DROP TRIGGER stock_correction_delete_guard ON stock_move_line_correction;
        DROP TRIGGER stock_correction_truncate_guard ON stock_move_line_correction;
        DROP TRIGGER stock_location_policy_insert ON stock_location;
        DROP TRIGGER stock_policy_update_guard ON stock_location_policy;
        DROP TRIGGER stock_policy_delete_guard ON stock_location_policy;
        DROP TRIGGER stock_policy_truncate_guard ON stock_location_policy;
        DROP FUNCTION stock_guard_done_at();
        DROP FUNCTION stock_guard_correction();
        DROP FUNCTION stock_create_location_policies();
        DROP FUNCTION stock_guard_location_policy();
        DROP TRIGGER stock_move_counts_insert ON stock_move;
        DROP TRIGGER stock_move_counts_update ON stock_move;
        DROP TRIGGER stock_move_counts_delete ON stock_move;
        DROP TRIGGER stock_picking_state_insert ON stock_picking;
        DROP TRIGGER stock_picking_state_update ON stock_picking;
        DROP TRIGGER stock_location_bypass_guard ON stock_location;
        DROP FUNCTION stock_move_counts();
        DROP FUNCTION stock_picking_eval_state();
        DROP FUNCTION stock_guard_location_bypass();
        DELETE FROM erp_schema_sql_object WHERE addon='stock';
        CREATE TEMP TABLE later_stock_security ON COMMIT DROP AS
            SELECT id,model,res_id FROM ir_model_data WHERE module='stock'
            AND model IN ('ir.access','res.groups','res.groups.privilege');
        DELETE FROM ir_access WHERE id IN (SELECT res_id FROM later_stock_security WHERE model='ir.access');
        DELETE FROM res_groups_implied_rel WHERE gid IN
            (SELECT res_id FROM later_stock_security WHERE model='res.groups') OR hid IN
            (SELECT res_id FROM later_stock_security WHERE model='res.groups');
        DELETE FROM res_groups WHERE id IN (SELECT res_id FROM later_stock_security WHERE model='res.groups');
        DELETE FROM res_groups_privilege WHERE id IN
            (SELECT res_id FROM later_stock_security WHERE model='res.groups.privilege');
        DELETE FROM ir_model_data WHERE id IN (SELECT id FROM later_stock_security);
        ALTER TABLE stock_picking_type DROP CONSTRAINT stock_picking_type_reservation_days_bounds;
        ALTER TABLE stock_picking_type DROP CONSTRAINT stock_picking_type_reservation_days_method;
        ALTER TABLE stock_picking_type ALTER COLUMN reservation_days_before DROP NOT NULL;
        ALTER TABLE stock_picking_type ALTER COLUMN reservation_days_before_priority DROP NOT NULL;
        ALTER TABLE stock_move ALTER COLUMN is_scrap DROP NOT NULL;
        ALTER TABLE stock_move ALTER COLUMN picked DROP NOT NULL;
        ALTER TABLE stock_move DROP CONSTRAINT stock_move_done_has_time;
        UPDATE ir_module_module SET latest_version='1.0.0' WHERE name='stock';").await.unwrap();
    client.execute("UPDATE stock_picking_type SET reservation_days_before=NULL,reservation_days_before_priority=NULL WHERE id=$1", &[&operations[0]]).await.unwrap();
    client.execute("UPDATE stock_picking_type SET reservation_method='by_date',reservation_days_before=365,reservation_days_before_priority=7 WHERE id=$1", &[&operations[1]]).await.unwrap();
    let gaps = client.query("INSERT INTO stock_location(name,usage,active) VALUES ('Legacy active gap','view',true),('Legacy archived gap','view',false) RETURNING id", &[]).await.unwrap().iter().map(|row| row.get(0)).collect();
    client.batch_execute("COMMIT").await.unwrap();
    gaps
}

/// The counter vector of a transfer and an independent fold of its moves (3: open
/// demand is non-zero demand, confirmed demand is positive demand), with its stored state.
const VECTOR_SQL: &str = "SELECT json_build_array(move_count,draft_count,waiting_count,confirmed_count,
    partially_available_count,assigned_count,done_count,cancel_count,done_scrap_count,cancel_scrap_count,
    assigned_zero_demand_count,open_positive_demand_count,confirmed_positive_demand_count,mto_nonscrap_count)::text,
    (SELECT json_build_array(count(*),count(*) FILTER (WHERE m.state='draft'),count(*) FILTER (WHERE m.state='waiting'),
        count(*) FILTER (WHERE m.state='confirmed'),count(*) FILTER (WHERE m.state='partially_available'),
        count(*) FILTER (WHERE m.state='assigned'),count(*) FILTER (WHERE m.state='done'),
        count(*) FILTER (WHERE m.state='cancel'),count(*) FILTER (WHERE m.state='done' AND m.is_scrap),
        count(*) FILTER (WHERE m.state='cancel' AND m.is_scrap),
        count(*) FILTER (WHERE m.state='assigned' AND m.product_uom_qty=0),
        count(*) FILTER (WHERE m.state NOT IN ('done','cancel') AND m.product_uom_qty<>0),
        count(*) FILTER (WHERE m.state='confirmed' AND m.product_uom_qty>0),
        count(*) FILTER (WHERE NOT coalesce(m.is_scrap,false) AND m.procure_method='make_to_order'))::text
     FROM stock_move m WHERE m.picking_id=p.id), state::text
    FROM stock_picking p WHERE id=$1";

/// Two transfers created before the legacy shape is restored: one leaving an internal location,
/// one leaving the vendors location (which bypasses reservation). Returns the transfers and what
/// the legacy moves need: company, product, unit, both sources.
async fn counted_pickings(orm: &Orm, operation: i64) -> ([i64; 2], [i64; 5]) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let unit: i64 = tx
        .query(
            "SELECT res_id FROM ir_model_data WHERE module='uom' AND name='product_uom_unit'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let vendors: i64 = tx.query("SELECT res_id FROM ir_model_data WHERE module='stock' AND name='stock_location_suppliers'", &[]).await.unwrap()[0].get(0);
    let env = tx.env().with_company(company).unwrap();
    let create = async |tx: &mut erp_sdk::Transaction, model: &str, vals: RawVals| -> i64 {
        env.records(model)
            .unwrap()
            .create(tx, vec![vals])
            .await
            .unwrap()
            .ids()[0]
    };
    let product = create(
        &mut tx,
        "product.product",
        RawVals::new().set_cell("name", Cell::Str("Counted widget".into())),
    )
    .await;
    let internal = create(
        &mut tx,
        "stock.location",
        RawVals::new().set_cell("name", Cell::Str("Counted stock".into())),
    )
    .await;
    let mut pickings = [0; 2];
    for (slot, (name, source)) in pickings
        .iter_mut()
        .zip([("LEGACY/1", internal), ("LEGACY/2", vendors)])
    {
        *slot = create(
            &mut tx,
            "stock.picking",
            RawVals::new()
                .set_cell("name", Cell::Str(name.into()))
                .set_cell("company_id", Cell::Id(company))
                .set_cell("picking_type_id", Cell::Id(operation))
                .set_cell("location_id", Cell::Id(source))
                .set_cell("location_dest_id", Cell::Id(internal)),
        )
        .await;
    }
    tx.commit().await.unwrap();
    (pickings, [company, product, unit, internal, vendors])
}

/// Moves written while no counter trigger exists, as before stock 1.6: the vectors stay zero. As
/// before 1.9, none stores its picked flag and one no scrap flag (NULL), and a done move outside
/// any transfer has no completion time.
async fn legacy_moves(client: &Client, pickings: [i64; 2], refs: [i64; 5]) {
    let [company, product, unit, internal, vendors] = refs;
    for (picking, source, state, qty, scrap) in [
        (Some(pickings[0]), internal, "assigned", 5, Some(false)),
        (Some(pickings[0]), internal, "confirmed", 5, None),
        (Some(pickings[1]), vendors, "confirmed", 5, Some(false)),
        (Some(pickings[1]), vendors, "cancel", 1, Some(true)),
        (None, internal, "done", 2, Some(false)),
    ] {
        client.execute(
            "INSERT INTO stock_move (company_id, product_id, uom_id, location_id, location_dest_id, date, state,
                procure_method, open_line_version, product_uom_qty, quantity, product_qty, quantity_product_uom,
                uom_factor, product_uom_factor, product_unit_digits, picking_id, is_scrap)
             VALUES ($1,$2,$3,$4,$5,now(),$6,'make_to_stock',0,$7::int4::numeric,0,$7::int4::numeric,0,1,1,2,$8,$9)",
            &[&company, &product, &unit, &source, &internal, &state, &qty, &picking, &scrap],
        ).await.unwrap();
    }
}

async fn assert_security(client: &Client, expected: &[String]) {
    let rows = client.query("SELECT d.name,d.res_id,g.id FROM ir_model_data d LEFT JOIN res_groups g ON g.id=d.res_id AND d.model='res.groups' WHERE d.module='stock' AND d.name=ANY($1) ORDER BY d.name", &[&GROUPS]).await.unwrap();
    let mut names: Vec<&str> = GROUPS.to_vec();
    names.sort_unstable();
    assert_eq!(rows.len(), names.len());
    let mut ids = std::collections::BTreeSet::new();
    for (row, name) in rows.iter().zip(names) {
        assert_eq!(row.get::<_, String>(0), name);
        let id: i64 = row.get(1);
        assert_eq!(row.get::<_, Option<i64>>(2), Some(id), "stock.{name}");
        assert!(
            ids.insert(id),
            "stock.{name} shares another group's identity"
        );
    }
    assert_eq!(security_contents(client).await, expected);
    for (from_module, from, to_module, to) in [
        ("stock", "group_stock_user", "base", "group_user"),
        ("stock", "group_stock_manager", "stock", "group_stock_user"),
        ("base", "group_system", "stock", "group_stock_manager"),
    ] {
        let count: i64 = client.query_one("SELECT count(*) FROM res_groups_implied_rel r JOIN ir_model_data f ON f.model='res.groups' AND f.res_id=r.gid JOIN ir_model_data t ON t.model='res.groups' AND t.res_id=r.hid WHERE f.module=$1 AND f.name=$2 AND t.module=$3 AND t.name=$4", &[&from_module,&from,&to_module,&to]).await.unwrap().get(0);
        assert_eq!(count, 1, "{from_module}.{from} -> {to_module}.{to}");
    }
    let rows = client.query("SELECT d.name,p.id FROM ir_model_data d JOIN res_groups_privilege p ON p.id=d.res_id AND d.model='res.groups.privilege' WHERE d.module='stock'", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<_, String>(0),
        "res_groups_privilege_inventory"
    );
    let privilege: i64 = rows[0].get(1);
    let roles = client.query("SELECT g.privilege_id FROM res_groups g JOIN ir_model_data d ON d.model='res.groups' AND d.res_id=g.id WHERE d.module='stock' AND d.name IN ('group_stock_user','group_stock_manager')", &[]).await.unwrap();
    assert_eq!(roles.len(), 2);
    for row in roles {
        assert_eq!(row.get::<_, Option<i64>>(0), Some(privilege));
    }
}

async fn assert_reservation_checks(orm: &Orm, id: i64) {
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let checks = tx.query("SELECT conname,convalidated FROM pg_constraint WHERE conrelid='stock_picking_type'::regclass AND conname IN ('stock_picking_type_reservation_days_bounds','stock_picking_type_reservation_days_method') ORDER BY conname", &[]).await.unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|row| row.get::<_, bool>(1)));
    let before: String = tx
        .query(
            "SELECT row_to_json(t)::text FROM stock_picking_type t WHERE id=$1",
            &[&id],
        )
        .await
        .unwrap()[0]
        .get(0);
    for field in [
        "reservation_days_before",
        "reservation_days_before_priority",
    ] {
        for value in [-1, 366] {
            let sql = format!(
                "UPDATE stock_picking_type SET reservation_method='by_date',{field}=$1 WHERE id=$2"
            );
            let error = tx
                .savepoint(async |tx| tx.execute(&sql, &[&value, &id]).await)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
            assert_eq!(
                error.constraint(),
                Some("stock_picking_type_reservation_days_bounds")
            );
            assert_eq!(
                error.message(),
                constraint_message(
                    "stock.picking.type reservation lead times must be between 0 and 365 days."
                )
            );
        }
        for method in ["manual", "at_confirm"] {
            let sql = format!(
                "UPDATE stock_picking_type SET reservation_method=$1,{field}=1 WHERE id=$2"
            );
            let error = tx
                .savepoint(async |tx| tx.execute(&sql, &[&method, &id]).await)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
            assert_eq!(
                error.constraint(),
                Some("stock_picking_type_reservation_days_method")
            );
            assert_eq!(
                error.message(),
                constraint_message(
                    "stock.picking.type reservation lead times must be zero unless reservation_method is by_date."
                )
            );
        }
    }
    let after: String = tx
        .query(
            "SELECT row_to_json(t)::text FROM stock_picking_type t WHERE id=$1",
            &[&id],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(after, before);
    for value in [0, 365] {
        tx.execute("UPDATE stock_picking_type SET reservation_method='by_date',reservation_days_before=$1,reservation_days_before_priority=$1 WHERE id=$2", &[&value, &id]).await.unwrap();
    }
    tx.rollback().await.unwrap();
}

#[test]
fn stock_upgrade_from_main_1_0_preserves_rows_and_repeats() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install(&db).await;
        let operations = operation_types(&installed).await;
        let (pickings, refs) = counted_pickings(&installed, operations[0]).await;
        let client = db.client().await.unwrap();
        let expected_security = security_contents(&client).await;
        assert_security(&client, &expected_security).await;
        let kept: i64 = client
            .query_one(
                "SELECT id FROM stock_location_policy ORDER BY id LIMIT 1",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        client.execute("UPDATE stock_location_policy SET forbid_negative=true,create_date=TIMESTAMP '2001-02-03 04:05:06',write_date=TIMESTAMP '2002-03-04 05:06:07' WHERE id=$1", &[&kept]).await.unwrap();
        let kept_image: String = client
            .query_one(
                "SELECT row_to_json(p)::text FROM stock_location_policy p WHERE id=$1",
                &[&kept],
            )
            .await
            .unwrap()
            .get(0);
        let gaps = restore_legacy_shape(&client, &operations).await;
        legacy_moves(&client, pickings, refs).await;
        for picking in pickings {
            let row = client.query_one(VECTOR_SQL, &[&picking]).await.unwrap();
            assert_eq!(
                row.get::<_, String>(0),
                "[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]",
                "legacy vector"
            );
            assert_eq!(row.get::<_, String>(2), "draft");
        }
        let legacy_ids = identities(&client).await;
        assert!(!legacy_ids.is_empty());
        assert_eq!(
            client
                .query_one(
                    "SELECT latest_version FROM ir_module_module WHERE name='stock'",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, String>(0),
            "1.0.0"
        );
        for sql in [
            "SELECT count(*) FROM erp_schema_sql_object WHERE addon='stock'",
            "SELECT count(*) FROM ir_model_data WHERE module='stock' AND model IN ('ir.access','res.groups','res.groups.privilege')",
            "SELECT count(*) FROM pg_trigger WHERE tgname='stock_location_policy_insert'",
        ] {
            assert_eq!(
                client.query_one(sql, &[]).await.unwrap().get::<_, i64>(0),
                0,
                "{sql}"
            );
        }
        assert_eq!(client.query_one("SELECT count(*) FROM stock_location l WHERE l.id=ANY($1) AND NOT EXISTS (SELECT FROM stock_location_policy p WHERE p.location_id=l.id)", &[&gaps]).await.unwrap().get::<_, i64>(0), i64::try_from(gaps.len()).unwrap());
        assert_eq!(gaps.len(), 2);
        drop(installed);

        let target = stock_manifest_version();
        let lower: String = client
            .query_one("SELECT (clock_timestamp() AT TIME ZONE 'UTC')::text", &[])
            .await
            .unwrap()
            .get(0);
        let sys = system(&db);
        let modules = [
            ModuleName::new("base").unwrap(),
            ModuleName::new("stock").unwrap(),
        ];
        let upgraded = sys
            .button_immediate_upgrade(&sys.system_env(Context::new()), &modules)
            .await
            .unwrap();
        assert!(
            upgraded.post_commit.is_clean(),
            "{:?}",
            upgraded.post_commit
        );
        let upper: String = client
            .query_one("SELECT (clock_timestamp() AT TIME ZONE 'UTC')::text", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            client
                .query_one(
                    "SELECT latest_version FROM ir_module_module WHERE name='stock'",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, String>(0),
            target
        );
        let rows = client.query("SELECT location_id,forbid_negative,create_uid,write_uid,create_date=write_date AND create_date BETWEEN $2::text::timestamp AND $3::text::timestamp AS utc_stamp FROM stock_location_policy WHERE location_id=ANY($1) ORDER BY location_id", &[&gaps, &lower, &upper]).await.unwrap();
        assert_eq!(rows.len(), gaps.len());
        for row in rows {
            assert!(!row.get::<_, bool>(1));
            assert_eq!(row.get::<_, Option<i64>>(2), None);
            assert_eq!(row.get::<_, Option<i64>>(3), None);
            assert_eq!(row.get::<_, Option<bool>>(4), Some(true));
        }
        assert_eq!(client.query_one("SELECT count(*) FROM stock_location l WHERE (SELECT count(*) FROM stock_location_policy p WHERE p.location_id=l.id)<>1", &[]).await.unwrap().get::<_, i64>(0), 0);
        assert_eq!(
            client
                .query_one(
                    "SELECT row_to_json(p)::text FROM stock_location_policy p WHERE id=$1",
                    &[&kept]
                )
                .await
                .unwrap()
                .get::<_, String>(0),
            kept_image
        );
        let current_ids = identities(&client).await;
        for row in legacy_ids {
            assert!(current_ids.contains(&row), "legacy identity changed: {row}");
        }
        let days = client.query("SELECT id,reservation_days_before,reservation_days_before_priority FROM stock_picking_type WHERE id=ANY($1) ORDER BY id", &[&operations]).await.unwrap();
        assert_eq!(days.len(), operations.len());
        for ((row, id), expected) in days.iter().zip(&operations).zip([(0, 0), (365, 7)]) {
            assert_eq!(row.get::<_, i64>(0), *id);
            assert_eq!((row.get::<_, i32>(1), row.get::<_, i32>(2)), expected);
        }
        assert_security(&client, &expected_security).await;
        // stock 1.9: both move flags are two-valued and required, a done move has its time
        for sql in [
            "SELECT count(*) FROM stock_move WHERE is_scrap IS NULL OR picked IS NULL",
            "SELECT count(*) FROM stock_move WHERE state = 'done' AND done_at IS NULL",
            "SELECT count(*) FROM information_schema.columns WHERE table_schema = current_schema() \
             AND table_name = 'stock_move' AND column_name IN ('is_scrap', 'picked') \
             AND is_nullable = 'YES'",
        ] {
            assert_eq!(
                client.query_one(sql, &[]).await.unwrap().get::<_, i64>(0),
                0,
                "{sql}"
            );
        }
        assert_eq!(
            client
                .query_one(
                    "SELECT count(*) FROM stock_move WHERE NOT is_scrap AND NOT picked",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, i64>(0),
            4
        );
        assert_eq!(
            client
                .query_one(
                    "SELECT count(*) FROM pg_constraint WHERE conname = 'stock_move_done_has_time'",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        for (picking, state) in pickings.into_iter().zip(["assigned", "assigned"]) {
            let row = client.query_one(VECTOR_SQL, &[&picking]).await.unwrap();
            assert_eq!(
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                "transfer {picking}"
            );
            assert_eq!(row.get::<_, String>(2), state, "transfer {picking}");
        }
        assert_reservation_checks(&upgraded.orm, operations[0]).await;
        // the overview card composes on the upgraded registry: declared as a same-addon extension
        // in a file sorting before the model's, it refused every registry holding stock
        let picking_type = upgraded.orm.registry().model("stock.picking.type").unwrap();
        assert!(picking_type.field("dashboard_card").is_some());
        let snapshot = stable_state(&client).await;
        drop(upgraded);
        let repeated = sys
            .button_immediate_upgrade(&sys.system_env(Context::new()), &modules)
            .await
            .unwrap();
        assert!(
            repeated.post_commit.is_clean(),
            "{:?}",
            repeated.post_commit
        );
        assert_eq!(stable_state(&client).await, snapshot);
        assert_eq!(
            client
                .query_one(
                    "SELECT latest_version FROM ir_module_module WHERE name='stock'",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, String>(0),
            target
        );
        let fresh: i64 = client.query_one("INSERT INTO stock_location(name,usage) VALUES ('After upgrade','view') RETURNING id", &[]).await.unwrap().get(0);
        assert_eq!(
            client
                .query_one(
                    "SELECT count(*) FROM stock_location_policy WHERE location_id=$1",
                    &[&fresh]
                )
                .await
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        drop(repeated);
        drop(sys);
        drop(client);
        db.cleanup().await.unwrap();
    });
}

/// Take the generated warehouses out again, as on a database that had stock 1.7 without one.
async fn drop_warehouses(client: &Client) {
    client
        .batch_execute(
            "BEGIN;
        CREATE TEMP TABLE gone_types ON COMMIT DROP AS
          SELECT id, sequence_id FROM stock_picking_type WHERE warehouse_id IS NOT NULL;
        CREATE TEMP TABLE gone_locations ON COMMIT DROP AS
          SELECT id FROM stock_location WHERE generated_warehouse_id IS NOT NULL;
        UPDATE stock_warehouse SET in_type_id = NULL, out_type_id = NULL, int_type_id = NULL,
          pick_type_id = NULL, pack_type_id = NULL, qc_type_id = NULL, store_type_id = NULL,
          xdock_type_id = NULL, delivery_route_id = NULL;
        DELETE FROM stock_route WHERE generated_warehouse_id IS NOT NULL
          AND generated_addon = 'stock' AND generated_role = 'delivery_route_id';
        DELETE FROM stock_picking_type WHERE id IN (SELECT id FROM gone_types);
        DELETE FROM ir_sequence WHERE id IN (SELECT sequence_id FROM gone_types);
        UPDATE stock_location SET generated_warehouse_id = NULL, generated_addon = NULL,
          generated_role = NULL, warehouse_id = NULL WHERE id IN (SELECT id FROM gone_locations);
        DELETE FROM stock_warehouse;
        DELETE FROM stock_location WHERE id IN (SELECT id FROM gone_locations) AND usage <> 'view';
        DELETE FROM stock_location WHERE id IN (SELECT id FROM gone_locations);
        UPDATE ir_module_module SET latest_version = '1.7.0' WHERE name = 'stock';
        COMMIT;",
        )
        .await
        .unwrap();
}

#[test]
fn stock_upgrade_from_1_7_gives_each_company_without_one_its_warehouse() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install(&db).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        let currency: i64 = tx
            .query(
                "SELECT currency_id FROM res_company ORDER BY id LIMIT 1",
                &[],
            )
            .await
            .unwrap()[0]
            .get(0);
        let second = tx
            .env()
            .records("res.company")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Second Company".into()))
                        .set_cell("currency_id", Cell::Id(currency)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.commit().await.unwrap();
        drop(installed);
        let client = db.client().await.unwrap();
        drop_warehouses(&client).await;
        assert_eq!(
            client
                .query_one("SELECT count(*) FROM stock_warehouse", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        let sys = system(&db);
        let modules = [ModuleName::new("stock").unwrap()];
        let upgraded = sys
            .button_immediate_upgrade(&sys.system_env(Context::new()), &modules)
            .await
            .unwrap();
        assert!(
            upgraded.post_commit.is_clean(),
            "{:?}",
            upgraded.post_commit
        );
        let mut tx = upgraded.orm.begin(SUPERUSER_ID).await.unwrap();
        let main: i64 = tx
            .query(
                "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
                &[],
            )
            .await
            .unwrap()[0]
            .get(0);
        for (company, name, code) in [
            (main, "Main Company", "Main"),
            (second, "Second Company", "Secon"),
        ] {
            let warehouse = super::warehouse_setup::warehouse_of(&mut tx, company).await;
            super::warehouse_setup::assert_topology(&mut tx, warehouse, name, code, "standard")
                .await;
        }
        let snapshot = super::warehouse_setup::topology_image(&mut tx).await;
        tx.rollback().await.unwrap();
        drop(tx);
        drop(upgraded);
        // a repeat of the upgrade creates and changes nothing
        client
            .execute(
                "UPDATE ir_module_module SET latest_version = '1.7.0' WHERE name = 'stock'",
                &[],
            )
            .await
            .unwrap();
        let repeated = sys
            .button_immediate_upgrade(&sys.system_env(Context::new()), &modules)
            .await
            .unwrap();
        assert!(
            repeated.post_commit.is_clean(),
            "{:?}",
            repeated.post_commit
        );
        let mut tx = repeated.orm.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(
            super::warehouse_setup::topology_image(&mut tx).await,
            snapshot
        );
        tx.rollback().await.unwrap();
        drop(tx);
        drop(repeated);
        // a company left without one at the current version (committed by an older node during an
        // upgrade) gets it at the next upgrade of Inventory, whatever its version
        let current: String = client
            .query_one(
                "SELECT latest_version FROM ir_module_module WHERE name = 'stock'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        drop_warehouses(&client).await;
        client
            .execute(
                "UPDATE ir_module_module SET latest_version = $1 WHERE name = 'stock'",
                &[&current],
            )
            .await
            .unwrap();
        let again = sys
            .button_immediate_upgrade(&sys.system_env(Context::new()), &modules)
            .await
            .unwrap();
        assert!(again.post_commit.is_clean(), "{:?}", again.post_commit);
        let mut tx = again.orm.begin(SUPERUSER_ID).await.unwrap();
        for company in [main, second] {
            super::warehouse_setup::warehouse_of(&mut tx, company).await;
        }
        tx.rollback().await.unwrap();
        drop(tx);
        drop(again);
        drop(sys);
        drop(client);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn stock_upgrade_from_1_12_fills_missing_direct_delivery_routes_once() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install(&db).await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        let main = tx
            .query(
                "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
                &[],
            )
            .await
            .unwrap()[0]
            .get::<_, i64>(0);
        let currency = tx
            .query(
                "SELECT currency_id FROM res_company WHERE id = $1",
                &[&main],
            )
            .await
            .unwrap()[0]
            .get::<_, i64>(0);
        let legacy = super::warehouse_setup::warehouse_of(&mut tx, main).await;
        let second = tx
            .env()
            .records("res.company")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Custom Shipment".into()))
                        .set_cell("currency_id", Cell::Id(currency)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let configured = super::warehouse_setup::warehouse_of(&mut tx, second).await;
        let env = tx.env().with_company(second).unwrap();
        let custom = env
            .records("stock.route")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Administrator delivery choice".into()))
                        .set_cell("company_id", Cell::Id(second)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.commit().await.unwrap();
        drop(tx);
        drop(installed);

        let client = db.client().await.unwrap();
        // Restore just the pre-1.13 shape of an existing warehouse and preserve a separate
        // configured route. This uses the current binary with a repaired legacy fixture.
        client
            .execute(
                "UPDATE stock_warehouse SET delivery_route_id = CASE WHEN id = $2 THEN $3::int8 END \
                 WHERE id IN ($1, $2)",
                &[&legacy, &configured, &custom],
            )
            .await
            .unwrap();
        client
            .execute(
                "DELETE FROM stock_route WHERE generated_warehouse_id = $1 AND generated_role = 'delivery_route_id'",
                &[&legacy],
            )
            .await
            .unwrap();
        let quantity_action = "SELECT a.params->>'report', quantity.res_id::text
             FROM ir_model_data shipped
             JOIN ir_act_client a ON a.id = shipped.res_id
             CROSS JOIN ir_model_data quantity
             WHERE shipped.module = 'stock' AND shipped.name = 'action_report_stock_position'
               AND quantity.module = 'stock' AND quantity.name = 'report_stock_quantity'";
        let original_action = client.query_one(quantity_action, &[]).await.unwrap();
        let quantity_report = original_action.get::<_, String>(1);
        assert_eq!(original_action.get::<_, String>(0), quantity_report);
        let sys = system(&db);
        let modules = [ModuleName::new("stock").unwrap()];
        let target = stock_manifest_version();
        let mut captured = None;
        for _ in 0..2 {
            client
                .execute(
                    "UPDATE ir_module_module SET latest_version = '1.12.0' WHERE name = 'stock'",
                    &[],
                )
                .await
                .unwrap();
            let upgraded = sys
                .button_immediate_upgrade(&sys.system_env(Context::new()), &modules)
                .await
                .unwrap();
            assert!(
                upgraded.post_commit.is_clean(),
                "{:?}",
                upgraded.post_commit
            );
            let installed_version = client
                .query_one(
                    "SELECT latest_version FROM ir_module_module WHERE name = 'stock'",
                    &[],
                )
                .await
                .unwrap()
                .get::<_, String>(0);
            assert_eq!(installed_version, target);
            let retained_action = client.query_one(quantity_action, &[]).await.unwrap();
            assert_eq!(retained_action.get::<_, String>(0), quantity_report);
            let row = client
                .query_one(
                    "SELECT (SELECT delivery_route_id FROM stock_warehouse WHERE id = $2), \
                       (SELECT r.id FROM stock_warehouse w JOIN stock_rule r ON r.route_id = w.delivery_route_id \
                        WHERE w.id = $1 AND r.warehouse_id = w.id AND r.company_id = w.company_id \
                          AND r.location_src_id = w.lot_stock_id AND r.picking_type_id = w.out_type_id \
                          AND r.action = 'pull' AND r.procure_method = 'make_to_stock' AND r.active)",
                    &[&legacy, &configured],
                )
                .await
                .unwrap();
            assert_eq!(row.get::<_, Option<i64>>(0), Some(custom));
            let rule = row.get::<_, Option<i64>>(1);
            assert!(
                rule.is_some(),
                "the existing one-step warehouse needs a delivery rule"
            );
            if let Some(previous) = captured {
                assert_eq!(rule, Some(previous));
            }
            captured = rule;
            drop(upgraded);
        }
        drop(client);
        db.cleanup().await.unwrap();
    });
}
