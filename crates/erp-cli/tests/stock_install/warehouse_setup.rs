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
