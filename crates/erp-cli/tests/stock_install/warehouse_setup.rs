// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every company's warehouse with its generated locations, operation types and numbering, and
//! what a new warehouse takes from its company, through the real installer.
use super::*;

async fn create(
    tx: &mut erp_orm::Transaction,
    env: &erp_orm::Env,
    model: &str,
    vals: RawVals,
) -> i64 {
    env.records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap_or_else(|e| panic!("create {model}: {e:?}"))
        .ids()[0]
}

async fn rows(tx: &mut erp_orm::Transaction, sql: &str, id: i64) -> Vec<String> {
    let rows = tx.query(sql, &[&id]).await.unwrap();
    rows.iter().map(|r| r.get::<_, String>(0)).collect()
}

/// The warehouse of `company` (exactly one).
pub(super) async fn warehouse_of(tx: &mut erp_orm::Transaction, company: i64) -> i64 {
    let rows = tx
        .query(
            "SELECT id FROM stock_warehouse WHERE company_id=$1",
            &[&company],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "company {company} warehouses");
    rows[0].get(0)
}

const WAREHOUSE: &str = "SELECT w.name || '|' || w.code || '|' || \
    (w.partner_id IS NOT DISTINCT FROM c.partner_id)::text || '|' || w.reception_steps || '|' || \
    w.delivery_steps || '|' || w.active::text \
    FROM stock_warehouse w JOIN res_company c ON c.id = w.company_id WHERE w.id = $1";

const LOCATIONS: &str = "SELECT l.generated_role || '|' || l.name || '|' || l.usage || '|' || \
    l.active::text || '|' || coalesce(l.barcode, '-') || '|' || l.replenish_location::text || '|' || \
    coalesce(p.generated_role, '-') || '|' || (l.warehouse_id IS NOT DISTINCT FROM w.id)::text || '|' || \
    (l.company_id = w.company_id)::text || '|' || (l.id = CASE l.generated_role \
      WHEN 'view_location_id' THEN w.view_location_id WHEN 'lot_stock_id' THEN w.lot_stock_id \
      WHEN 'wh_input_stock_loc_id' THEN w.wh_input_stock_loc_id \
      WHEN 'wh_qc_stock_loc_id' THEN w.wh_qc_stock_loc_id \
      WHEN 'wh_output_stock_loc_id' THEN w.wh_output_stock_loc_id \
      WHEN 'wh_pack_stock_loc_id' THEN w.wh_pack_stock_loc_id END)::text \
    FROM stock_location l JOIN stock_warehouse w ON w.id = l.generated_warehouse_id \
    LEFT JOIN stock_location p ON p.id = l.location_id \
    WHERE w.id = $1 AND l.generated_addon = 'stock' ORDER BY l.generated_role COLLATE \"C\"";

const TYPES: &str = "SELECT t.generated_role || '|' || (t.name->>'en_US') || '|' || t.code || '|' || \
    t.active::text || '|' || coalesce(t.barcode, '-') || '|' || s.prefix || '|' || s.padding::text || '|' || \
    s.implementation || '|' || (s.company_id = w.company_id)::text || '|' || \
    coalesce(src.generated_role, src.name) || '|' || coalesce(dst.generated_role, dst.name) || '|' || \
    coalesce(ret.generated_role, '-') || '|' || (t.id = CASE t.generated_role \
      WHEN 'in_type_id' THEN w.in_type_id WHEN 'out_type_id' THEN w.out_type_id \
      WHEN 'int_type_id' THEN w.int_type_id WHEN 'pick_type_id' THEN w.pick_type_id \
      WHEN 'pack_type_id' THEN w.pack_type_id WHEN 'qc_type_id' THEN w.qc_type_id \
      WHEN 'store_type_id' THEN w.store_type_id WHEN 'xdock_type_id' THEN w.xdock_type_id END)::text \
    || '|' || (t.company_id = w.company_id)::text || '|' || (t.generated_addon = 'stock')::text \
    FROM stock_picking_type t JOIN stock_warehouse w ON w.id = t.warehouse_id \
    JOIN ir_sequence s ON s.id = t.sequence_id \
    JOIN stock_location src ON src.id = t.default_location_src_id \
    JOIN stock_location dst ON dst.id = t.default_location_dest_id \
    LEFT JOIN stock_picking_type ret ON ret.id = t.return_picking_type_id \
    WHERE w.id = $1 ORDER BY t.generated_role COLLATE \"C\"";

/// The one-step topology of a warehouse with short name `code` (to 028, 030, 110),
/// its types numbered by `numbering`: `standard` when an install or upgrade made them, `no_gap` when
/// they were made while the server runs.
fn expected(code: &str, numbering: &str) -> (Vec<String>, Vec<String>) {
    let p = code.replace(' ', "").to_uppercase();
    let l = |role: &str, name: &str, usage: &str, active: bool, barcode: &str, parent: &str| {
        let replenish = role == "lot_stock_id";
        format!("{role}|{name}|{usage}|{active}|{barcode}|{replenish}|{parent}|true|true|true")
    };
    let locations = vec![
        l(
            "lot_stock_id",
            "Stock",
            "internal",
            true,
            &format!("{p}STOCK"),
            "view_location_id",
        ),
        l("view_location_id", code, "view", true, "-", "-"),
        l(
            "wh_input_stock_loc_id",
            "Inbound",
            "internal",
            false,
            &format!("{p}INPUT"),
            "view_location_id",
        ),
        l(
            "wh_output_stock_loc_id",
            "Outbound",
            "internal",
            false,
            &format!("{p}OUTPUT"),
            "view_location_id",
        ),
        l(
            "wh_pack_stock_loc_id",
            "Packing Area",
            "internal",
            false,
            &format!("{p}PACKING"),
            "view_location_id",
        ),
        l(
            "wh_qc_stock_loc_id",
            "Quality Check",
            "internal",
            false,
            &format!("{p}QUALITY"),
            "view_location_id",
        ),
    ];
    let t = |role: &str,
             name: &str,
             kind: &str,
             active: bool,
             short: &str,
             ends: (&str, &str),
             ret: &str| {
        let (src, dst) = ends;
        format!(
            "{role}|{name}|{kind}|{active}|{p}{short}|{code}/{short}/|5|{numbering}|true|{src}|{dst}|{ret}|true|true|true"
        )
    };
    let types = vec![
        t(
            "in_type_id",
            "Receiving",
            "incoming",
            true,
            "IN",
            ("Vendors", "lot_stock_id"),
            "out_type_id",
        ),
        t(
            "int_type_id",
            "Internal Moves",
            "internal",
            false,
            "INT",
            ("lot_stock_id", "lot_stock_id"),
            "-",
        ),
        t(
            "out_type_id",
            "Deliveries",
            "outgoing",
            true,
            "OUT",
            ("lot_stock_id", "Customers"),
            "in_type_id",
        ),
        t(
            "pack_type_id",
            "Packing",
            "internal",
            false,
            "PACK",
            ("wh_pack_stock_loc_id", "lot_stock_id"),
            "-",
        ),
        t(
            "pick_type_id",
            "Picking",
            "internal",
            false,
            "PICK",
            ("lot_stock_id", "wh_pack_stock_loc_id"),
            "-",
        ),
        t(
            "qc_type_id",
            "Quality Check",
            "internal",
            false,
            "QC",
            ("wh_input_stock_loc_id", "wh_qc_stock_loc_id"),
            "-",
        ),
        t(
            "store_type_id",
            "Put Away",
            "internal",
            false,
            "STOR",
            ("wh_qc_stock_loc_id", "lot_stock_id"),
            "-",
        ),
        t(
            "xdock_type_id",
            "Cross-Docking",
            "internal",
            false,
            "XD",
            ("wh_input_stock_loc_id", "wh_output_stock_loc_id"),
            "-",
        ),
    ];
    (locations, types)
}

/// Assert the generated one-step topology of `warehouse`, named `name` with short name `code`, its
/// types numbered by `numbering` (see [`expected`]).
pub(super) async fn assert_topology(
    tx: &mut erp_orm::Transaction,
    warehouse: i64,
    name: &str,
    code: &str,
    numbering: &str,
) {
    assert_eq!(
        rows(tx, WAREHOUSE, warehouse).await,
        [format!("{name}|{code}|true|one_step|ship_only|true")]
    );
    let (locations, types) = expected(code, numbering);
    assert_eq!(rows(tx, LOCATIONS, warehouse).await, locations);
    assert_eq!(rows(tx, TYPES, warehouse).await, types);
}

/// Every row the provisioning writes, to prove a repeat changes nothing.
pub(super) async fn topology_image(tx: &mut erp_orm::Transaction) -> Vec<String> {
    let mut image = Vec::new();
    for table in [
        "stock_warehouse",
        "stock_location",
        "stock_picking_type",
        "stock_route",
        "stock_rule",
        "ir_sequence",
    ] {
        let sql = format!("SELECT row_to_json(t)::text FROM {table} t ORDER BY id");
        image.extend(
            tx.query(&sql, &[])
                .await
                .unwrap()
                .iter()
                .map(|r| r.get::<_, String>(0)),
        );
    }
    image
}

#[test]
fn install_gives_every_company_its_warehouse_and_a_receipt_can_be_drafted() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let counts = tx
            .query(
                "SELECT count(w.id)::int8 FROM res_company c LEFT JOIN stock_warehouse w \
                 ON w.company_id = c.id GROUP BY c.id",
                &[],
            )
            .await
            .unwrap();
        assert!(!counts.is_empty());
        assert!(counts.iter().all(|r| r.get::<_, i64>(0) == 1));
        let company: i64 = tx
            .query(
                "SELECT res_id FROM ir_model_data WHERE module='base' AND name='main_company'",
                &[],
            )
            .await
            .unwrap()[0]
            .get(0);
        let name: String = tx
            .query("SELECT name FROM res_company WHERE id=$1", &[&company])
            .await
            .unwrap()[0]
            .get(0);
        assert_eq!(name, "Main Company");
        let warehouse = warehouse_of(&mut tx, company).await;
        assert_topology(&mut tx, warehouse, "Main Company", "Main", "standard").await;
        // the overview shows the active types: receipts and deliveries
        assert_eq!(
            rows(
                &mut tx,
                "SELECT name->>'en_US' FROM stock_picking_type WHERE active AND warehouse_id=$1 \
                 ORDER BY sequence, id",
                warehouse
            )
            .await,
            ["Receiving", "Deliveries"]
        );
        // a draft receipt against the generated receipt type
        let receipt = tx
            .query(
                "SELECT id, default_location_src_id, default_location_dest_id \
                 FROM stock_picking_type WHERE id = (SELECT in_type_id FROM stock_warehouse WHERE id=$1)",
                &[&warehouse],
            )
            .await
            .unwrap();
        let (kind, source, dest): (i64, i64, i64) =
            (receipt[0].get(0), receipt[0].get(1), receipt[0].get(2));
        let env = tx.env().with_company(company).unwrap();
        let picking = create(
            &mut tx,
            &env,
            "stock.picking",
            RawVals::new()
                .set_cell("name", Cell::Str("Main/IN/00001".into()))
                .set_cell("company_id", Cell::Id(company))
                .set_cell("picking_type_id", Cell::Id(kind))
                .set_cell("location_id", Cell::Id(source))
                .set_cell("location_dest_id", Cell::Id(dest)),
        )
        .await;
        assert_eq!(
            rows(
                &mut tx,
                "SELECT state FROM stock_picking WHERE id=$1",
                picking
            )
            .await,
            ["draft"]
        );
        // the install step again creates and changes nothing
        let before = topology_image(&mut tx).await;
        tx.env()
            .records("res.company")
            .unwrap()
            .call::<(), ()>(&mut tx, "_stock_create_missing_warehouses", ())
            .await
            .unwrap();
        assert_eq!(topology_image(&mut tx).await, before);
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn ship_only_warehouse_provisions_an_owned_delivery_route() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let company = super::security::xid(&orm, "base", "main_company").await;
        let customers = super::security::xid(&orm, "stock", "stock_location_customers").await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let env = tx.env().with_company(company).unwrap();
        let main = warehouse_of(&mut tx, company).await;
        let warehouse_values = |name: &str, code: &str| {
            RawVals::new()
                .set_cell("name", Cell::Str(name.into()))
                .set_cell("code", Cell::Str(code.into()))
                .set_cell("company_id", Cell::Id(company))
        };
        let new = create(
            &mut tx,
            &env,
            "stock.warehouse",
            warehouse_values("Second one-step site", "SHIP2"),
        )
        .await;
        let multi = create(
            &mut tx,
            &env,
            "stock.warehouse",
            warehouse_values("Two-step site", "PICK2")
                .set_cell("delivery_steps", Cell::Str("pick_ship".into())),
        )
        .await;
        let custom_route = create(
            &mut tx,
            &env,
            "stock.route",
            RawVals::new()
                .set_cell("name", Cell::Str("Explicit delivery route".into()))
                .set_cell("company_id", Cell::Id(company)),
        )
        .await;
        let custom = create(
            &mut tx,
            &env,
            "stock.warehouse",
            warehouse_values("Explicit route site", "CUSTOM")
                .set_cell("delivery_route_id", Cell::Id(custom_route)),
        )
        .await;
        tx.flush().await.unwrap();

        let mut generated_routes = Vec::new();
        for warehouse in [main, new] {
            let route = tx
                .query(
                    "SELECT r.id, r.company_id, s.action, s.procure_method,
                            s.location_src_id = w.lot_stock_id, s.location_dest_id,
                            s.picking_type_id = w.out_type_id, s.company_id, s.warehouse_id,
                            r.active AND s.active, s.generated_warehouse_id, s.generated_addon,
                            s.generated_role, t.code, s.location_dest_id = t.default_location_dest_id
                     FROM stock_warehouse w JOIN stock_route r ON r.id = w.delivery_route_id
                     JOIN stock_route_warehouse rw ON rw.route_id = r.id AND rw.warehouse_id = w.id
                     JOIN stock_rule s ON s.route_id = r.id
                     JOIN stock_picking_type t ON t.id = s.picking_type_id
                     WHERE w.id = $1 AND r.generated_warehouse_id = w.id
                       AND r.generated_addon = 'stock' AND r.generated_role = 'delivery_route_id'",
                    &[&warehouse],
                )
                .await
                .unwrap();
            assert_eq!(
                route.len(),
                1,
                "warehouse {warehouse} has one direct delivery rule"
            );
            let r = &route[0];
            generated_routes.push(r.get::<_, i64>(0));
            assert_eq!(r.get::<_, i64>(1), company);
            assert_eq!(r.get::<_, String>(2), "pull");
            assert_eq!(r.get::<_, String>(3), "make_to_stock");
            assert!(r.get::<_, bool>(4));
            assert_eq!(r.get::<_, i64>(5), customers);
            assert!(r.get::<_, bool>(6));
            assert_eq!(r.get::<_, i64>(7), company);
            assert_eq!(r.get::<_, i64>(8), warehouse);
            assert!(r.get::<_, bool>(9));
            assert_eq!(r.get::<_, i64>(10), warehouse);
            assert_eq!(r.get::<_, String>(11), "stock");
            assert_eq!(r.get::<_, String>(12), "ship_only_delivery");
            assert_eq!(r.get::<_, String>(13), "outgoing");
            assert!(r.get::<_, bool>(14));
        }
        assert_ne!(generated_routes[0], generated_routes[1]);

        let warehouses = env.records("stock.warehouse").unwrap();
        assert_eq!(
            warehouses
                .browse([custom])
                .unwrap()
                .cell(&mut tx, "delivery_route_id")
                .await
                .unwrap(),
            Cell::Id(custom_route)
        );
        assert_eq!(
            warehouses
                .browse([multi])
                .unwrap()
                .cell(&mut tx, "delivery_route_id")
                .await
                .unwrap(),
            Cell::Null
        );
        assert!(
            tx.query(
                "SELECT id FROM stock_route WHERE generated_warehouse_id = ANY($1)",
                &[&[custom, multi].as_slice()],
            )
            .await
            .unwrap()
            .is_empty()
        );
        let snapshot = topology_image(&mut tx).await;
        env.records("res.company")
            .unwrap()
            .call::<(), ()>(&mut tx, "_stock_create_missing_warehouses", ())
            .await
            .unwrap();
        assert_eq!(topology_image(&mut tx).await, snapshot);

        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn ship_only_warehouse_topology_preserves_employee_permissions() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let installed = install(&db).await;
        let users = super::security::roles(&installed).await;
        let orm = super::security::secure(&db, &installed).await;
        let company = super::security::xid(&installed, "base", "main_company").await;
        let mut tx = orm
            .begin(erp_orm::UserId::new(users[1]).unwrap())
            .await
            .unwrap();
        let warehouse = warehouse_of(&mut tx, company).await;
        let env = tx.env().with_company(company).unwrap();
        let warehouses = env.records("stock.warehouse").unwrap();
        let current = warehouses.browse([warehouse]).unwrap();
        let route = current.related(&mut tx, "delivery_route_id").await.unwrap();
        assert_eq!(route.ids().len(), 1);
        let rule = route.related(&mut tx, "rule_ids").await.unwrap();
        assert_eq!(rule.ids().len(), 1);
        assert_eq!(
            rule.cell(&mut tx, "action").await.unwrap(),
            Cell::Str("pull".into())
        );

        let before = topology_image(&mut tx).await;
        let expected =
            super::stock_access::refusal(&orm, "create", "c", "Warehouse", "stock.warehouse").await;
        let denied = warehouses
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("No employee warehouse".into()))
                        .set_cell("code", Cell::Str("DENY".into()))
                        .set_cell("company_id", Cell::Id(company)),
                ],
            )
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Access);
        assert_eq!(denied.message(), expected);
        assert_eq!(topology_image(&mut tx).await, before);
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        drop(installed);
        db.cleanup().await.unwrap();
    });
}

#[test]
fn a_new_company_gets_its_warehouse_named_after_it() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let env = tx.env();
        let currency: i64 = tx
            .query(
                "SELECT currency_id FROM res_company ORDER BY id LIMIT 1",
                &[],
            )
            .await
            .unwrap()[0]
            .get(0);
        let company = |name: &str| {
            RawVals::new()
                .set_cell("name", Cell::Str(name.into()))
                .set_cell("currency_id", Cell::Id(currency))
        };
        let northwind = create(&mut tx, &env, "res.company", company("Northwind Traders")).await;
        let partner: Option<i64> = tx
            .query(
                "SELECT partner_id FROM res_company WHERE id=$1",
                &[&northwind],
            )
            .await
            .unwrap()[0]
            .get(0);
        let partner = partner.expect("a new company has its contact");
        let warehouse = warehouse_of(&mut tx, northwind).await;
        assert_topology(&mut tx, warehouse, "Northwind Traders", "North", "no_gap").await;
        // an administrator shortens the derived short name
        env.records("ir.config_parameter")
            .unwrap()
            .call::<(String, Option<i64>), i64>(
                &mut tx,
                "set_int",
                ("stock.warehouse_code_length".to_owned(), Some(3)),
            )
            .await
            .unwrap();
        let contoso = create(&mut tx, &env, "res.company", company("Contoso Ltd")).await;
        let warehouse = warehouse_of(&mut tx, contoso).await;
        assert_topology(&mut tx, warehouse, "Contoso Ltd", "Con", "no_gap").await;
        // a warehouse with its own locations keeps them and still takes the company's values
        let mut roots = Vec::new();
        for name in ["R1", "R2"] {
            roots.push(
                create(
                    &mut tx,
                    &env,
                    "stock.location",
                    RawVals::new()
                        .set_cell("name", Cell::Str(name.into()))
                        .set_cell("usage", Cell::Str("view".into()))
                        .set_cell("company_id", Cell::Id(northwind)),
                )
                .await,
            );
        }
        let rooted = |root: i64| {
            RawVals::new()
                .set_cell("view_location_id", Cell::Id(root))
                .set_cell("lot_stock_id", Cell::Id(root))
        };
        let shape = "SELECT name || '|' || code || '|' || company_id::text || '|' || \
                     coalesce(partner_id::text, '-') || '|' || \
                     (SELECT count(*) FROM stock_picking_type t WHERE t.warehouse_id = w.id)::text \
                     FROM stock_warehouse w WHERE id=$1";
        let second = create(
            &mut tx,
            &env,
            "stock.warehouse",
            rooted(roots[0])
                .set_cell("company_id", Cell::Id(northwind))
                .set_cell("name", Cell::Str("Second site".into())),
        )
        .await;
        assert_eq!(
            rows(&mut tx, shape, second).await,
            [format!("Second site|Nor|{northwind}|{partner}|0")]
        );
        // without a company the environment's company is used
        let local = env.with_company(northwind).unwrap();
        let third = create(
            &mut tx,
            &local,
            "stock.warehouse",
            rooted(roots[1])
                .set_cell("name", Cell::Str("Third site".into()))
                .set_cell("code", Cell::Str("T3".into())),
        )
        .await;
        assert_eq!(
            rows(&mut tx, shape, third).await,
            [format!("Third site|T3|{northwind}|{partner}|0")]
        );
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}
