// SPDX-License-Identifier: MIT OR Apache-2.0
//! Schema of the stock operations tables: reference,
//! picking and move. Schema only: the rows below are written with plain SQL or plain ORM creates, because
//! the units that maintain counters, states, names and product-unit quantities are later slices.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use erp_addon_uom::conversion::Unit;
use erp_addon_uom::stock::{ProductUnitPreview, product_quantity_sql};
use erp_sdk::ErrorKind;
use erp_sdk::prelude::*;
use erp_testkit::Harness;

erp_sdk::addon!(name = "test_stock_ops", depends = ["stock"]);

/// A test stand-in for `ir.model.data` with the columns the product addon and the company create path
/// read (the real model lives in `erp-data`, out of reach of an addon crate; the product tests use the
/// same stand-in).
#[model(
    name = "ir.model.data",
    description = "Model Data",
    addon = "test_stock_ops"
)]
pub struct ModelData {
    #[field(char, required = true)]
    pub name: Option<Arc<str>>,
    #[field(char, required = true)]
    pub model: Option<Arc<str>>,
    #[field(char, required = true)]
    pub module: Option<Arc<str>>,
    #[field(many2one_reference, model_field = "model")]
    pub res_id: i64,
    /// Read by the company create path.
    #[field(boolean)]
    pub noupdate: bool,
}

/// The prefix the ORM puts before a declared constraint's message.
const PREFIX: &str = "The database refused the change: ";

/// Ids every case starts from: two companies with one operation type each, a shared product, a unit,
/// two shared locations, a shared partner.
struct Fixture {
    h: Harness,
    company: i64,
    other_company: i64,
    product: i64,
    unit: i64,
    location: i64,
    other_location: i64,
    partner: i64,
    picking_type: i64,
    other_picking_type: i64,
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

fn dec(value: &str) -> Cell {
    Cell::Decimal(Decimal::from_str_exact(value).unwrap())
}

async fn create(tx: &mut Transaction, model: &str, vals: RawVals) -> i64 {
    tx.env()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap_or_else(|e| panic!("create {model}: {e:?}"))
        .rec_ids()[0]
        .get()
}

/// An operation type of `company` between the two fixture locations.
async fn create_picking_type(
    tx: &mut Transaction,
    company: i64,
    name: &str,
    source: i64,
    dest: i64,
) -> i64 {
    create(
        tx,
        "stock.picking.type",
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("company_id", Cell::Id(company))
            .set_cell("default_location_src_id", Cell::Id(source))
            .set_cell("default_location_dest_id", Cell::Id(dest)),
    )
    .await
}

async fn fixture() -> Fixture {
    let h = Harness::builder([
        &erp_addon_base::ADDON,
        &erp_addon_web::ADDON,
        &erp_addon_barcodes::ADDON,
        &erp_addon_barcodes_gs1_nomenclature::ADDON,
        &erp_addon_uom::ADDON,
        &erp_addon_product::ADDON,
        &erp_addon_stock::ADDON,
        &erp_addon_reporting::ADDON,
        &ADDON,
    ])
    .require_sql_objects([&erp_addon_stock::schema_guards::MANIFEST])
    .schema_sync_options(erp_testkit::SchemaSyncOptions::inline().lifecycle(true))
    .build()
    .await
    .unwrap();
    let mut tx = h.begin().await.unwrap();
    tx.batch(include_str!("../../uom/src/guard_sql.sql"))
        .await
        .unwrap();
    tx.execute("INSERT INTO uom_reference_fence(singleton) VALUES (1)", &[])
        .await
        .unwrap();
    let unit = create(
        &mut tx,
        "uom.uom",
        RawVals::new()
            .set_cell("name", text("Units"))
            .set_cell("relative_factor", Cell::Decimal(Decimal::ONE)),
    )
    .await;
    create(
        &mut tx,
        "ir.model.data",
        RawVals::new()
            .set_cell("module", text("uom"))
            .set_cell("name", text("product_uom_unit"))
            .set_cell("model", text("uom.uom"))
            .set_cell("res_id", Cell::Id(unit)),
    )
    .await;
    // the companies' currency: an existing one, else a new one (as the product tests do)
    let existing = tx
        .query("SELECT id FROM res_currency ORDER BY id LIMIT 1", &[])
        .await
        .unwrap();
    let currency = match existing.first() {
        Some(row) => row.get::<_, i64>(0),
        None => {
            create(
                &mut tx,
                "res.currency",
                RawVals::new()
                    .set_cell("name", text("USD"))
                    .set_cell("symbol", text("$"))
                    .set_cell("rounding", Cell::Decimal(Decimal::new(1, 2))),
            )
            .await
        }
    };
    // a harness starts without companies
    let company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Main Company"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let other_company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Other Company"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    // the acting user is the default responsible of a transfer; it belongs to both companies, as
    // an administrator does (res.users is scoped by company_ids)
    let uid = tx.env().uid();
    for cid in [company, other_company] {
        tx.execute(
            // creating a company may already have added the acting user
            "INSERT INTO res_company_users_rel (user_id, cid) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            &[&uid, &cid],
        )
        .await
        .unwrap();
    }
    let env = tx.env().with_company(company).unwrap();
    let template = env
        .records("product.template")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("name", text("Widget"))],
        )
        .await
        .unwrap()
        .rec_ids()[0]
        .get();
    let product: i64 = tx
        .query(
            "SELECT id FROM product_product WHERE product_tmpl_id = $1",
            &[&template],
        )
        .await
        .unwrap()[0]
        .get(0);
    let location = create(
        &mut tx,
        "stock.location",
        RawVals::new().set_cell("name", text("Stock")),
    )
    .await;
    let other_location = create(
        &mut tx,
        "stock.location",
        RawVals::new().set_cell("name", text("Customers")),
    )
    .await;
    let partner = create(
        &mut tx,
        "res.partner",
        RawVals::new().set_cell("name", text("Consignor")),
    )
    .await;
    let picking_type =
        create_picking_type(&mut tx, company, "Receipts", location, other_location).await;
    let other_picking_type = create_picking_type(
        &mut tx,
        other_company,
        "Receipts B",
        location,
        other_location,
    )
    .await;
    tx.commit().await.unwrap();
    Fixture {
        h,
        company,
        other_company,
        product,
        unit,
        location,
        other_location,
        partner,
        picking_type,
        other_picking_type,
    }
}

/// `(column, data type, nullable)` of the named columns of `table`, in the order asked.
async fn columns(tx: &mut Transaction, table: &str, names: &[&str]) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for name in names {
        let rows = tx
            .query(
                "SELECT data_type::text, is_nullable::text FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2",
                &[&table, name],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "column {table}.{name} exists once");
        let nullable: String = rows[0].get(1);
        out.push(((*name).to_owned(), rows[0].get(0), nullable == "YES"));
    }
    out
}

fn expect(spec: &[(&str, &str, bool)]) -> Vec<(String, String, bool)> {
    spec.iter()
        .map(|(n, t, null)| ((*n).to_owned(), (*t).to_owned(), *null))
        .collect()
}

/// The definition of index `name`, from `USING` on (the schema prefix before it varies per test
/// database).
async fn index_definition(tx: &mut Transaction, name: &str) -> String {
    let rows = tx
        .query(
            "SELECT pg_get_indexdef(c.oid) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND c.relname = $1",
            &[&name],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "index {name} exists");
    let def: String = rows[0].get(0);
    def.split_once(" USING ")
        .expect("an index definition")
        .1
        .to_owned()
}

/// PostgreSQL's own rendering of an index on `table` written as `body` (`(cols) WHERE ...`): a probe
/// index is created, read and dropped again, so the plan's definition is compared in the catalog's
/// normal form rather than as a hand-typed string.
async fn probe_definition(tx: &mut Transaction, table: &str, body: &str) -> String {
    let probe = format!("ops_probe_{table}");
    tx.batch(&format!("CREATE INDEX {probe} ON {table} {body}"))
        .await
        .unwrap();
    let out = index_definition(tx, &probe).await;
    tx.batch(&format!("DROP INDEX {probe}")).await.unwrap();
    out
}

/// Assert the field index of `table.column` is a plain btree, or a btree over non-null values when
/// `not_null`, in PostgreSQL's rendering of the same definition.
async fn assert_btree(tx: &mut Transaction, table: &str, column: &str, not_null: bool) {
    let body = if not_null {
        format!("({column}) WHERE {column} IS NOT NULL")
    } else {
        format!("({column})")
    };
    let expected = probe_definition(tx, table, &body).await;
    assert_eq!(
        index_definition(tx, &format!("{table}__{column}_index")).await,
        expected,
        "{table}.{column}"
    );
}

/// Assert the field index of `table.column` is a trigram GIN index over the column.
async fn assert_trigram(tx: &mut Transaction, table: &str, column: &str) {
    let def = index_definition(tx, &format!("{table}__{column}_index")).await;
    assert!(
        def.starts_with("gin (") && def.contains(column) && def.contains("gin_trgm_ops"),
        "{table}.{column}: {def}"
    );
}

/// The indexes of relation table `table` as `(is primary key, key columns in order)`.
async fn relation_indexes(tx: &mut Transaction, table: &str) -> Vec<(bool, Vec<String>)> {
    let rows = tx
        .query(
            "SELECT i.indisprimary, array_agg(a.attname::text ORDER BY k.ord) \
             FROM pg_index i \
             JOIN pg_class c ON c.oid = i.indrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
             WHERE n.nspname = current_schema() AND c.relname = $1 \
             GROUP BY i.indexrelid, i.indisprimary ORDER BY i.indisprimary DESC",
            &[&table],
        )
        .await
        .unwrap();
    rows.iter().map(|r| (r.get(0), r.get(1))).collect()
}

/// Relation `table` has the two `bigint NOT NULL` columns, a primary key on both and one more index on
/// the reverse order. Which declaration creates the table (and so which column leads the primary key)
/// depends on registry order, so only the pair and the reversal are asserted.
async fn assert_relation(tx: &mut Transaction, table: &str, a: &str, b: &str) {
    assert_eq!(
        columns(tx, table, &[a, b]).await,
        expect(&[(a, "bigint", false), (b, "bigint", false)]),
        "{table}"
    );
    let indexes = relation_indexes(tx, table).await;
    assert_eq!(indexes.len(), 2, "{table}: {indexes:?}");
    let (primary, pk) = &indexes[0];
    let (other_primary, reverse) = &indexes[1];
    assert!(*primary && !*other_primary, "{table}: {indexes:?}");
    let mut pair = pk.clone();
    pair.sort();
    let mut wanted = vec![a.to_owned(), b.to_owned()];
    wanted.sort();
    assert_eq!(pair, wanted, "{table}");
    let reversed: Vec<String> = pk.iter().rev().cloned().collect();
    assert_eq!(*reverse, reversed, "{table}");
}

fn assert_violation(error: &Error, sqlstate: &str, constraint: &str, message: &str) {
    assert_eq!(error.sqlstate(), Some(sqlstate), "{error:?}");
    assert_eq!(error.constraint(), Some(constraint), "{error:?}");
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert_eq!(error.message(), format!("{PREFIX}{message}"));
}

const PICK_NONNEG: &str = "The move counts of a transfer cannot be negative.";
const PICK_SUM: &str = "The state counts of a transfer must add up to its move count.";
const PICK_SUBSETS: &str = "A subset count of a transfer cannot exceed the count it belongs to.";

/// The declared message of a move check (the model's text, `addons/stock/src/stock_move.rs`).
fn move_message(constraint: &str) -> &'static str {
    match constraint {
        "stock_move_digits_range" => {
            "The Product Unit digits of a stock move must be between 0 and 28."
        }
        "stock_move_factor_nonzero" => "The unit factors of a stock move cannot be zero.",
        "stock_move_finite_product_uom_qty" => {
            "The demand of a stock move must be a finite number."
        }
        "stock_move_finite_quantity" => "The quantity of a stock move must be a finite number.",
        "stock_move_finite_product_qty" => {
            "The demand in the product unit of a stock move must be a finite number."
        }
        "stock_move_finite_quantity_product_uom" => {
            "The quantity in the product unit of a stock move must be a finite number."
        }
        "stock_move_finite_uom_factor" => {
            "The unit factor of a stock move must be a finite number."
        }
        "stock_move_finite_product_uom_factor" => {
            "The product unit factor of a stock move must be a finite number."
        }
        "stock_move_finite_price_unit" => "The unit price of a stock move must be a finite number.",
        "stock_move_finite_packaging_uom_qty" => {
            "The packaging quantity of a stock move must be a finite number."
        }
        "stock_move_open_line_version_nonneg" => {
            "The open line version of a stock move cannot be negative."
        }
        "stock_move_product_qty_conversion" => {
            "The demand in the product unit must be the demand converted with the stored unit factors and digits."
        }
        "stock_move_quantity_product_uom_conversion" => {
            "The quantity in the product unit must be the quantity converted with the stored unit factors and digits."
        }
        other => panic!("no move check {other}"),
    }
}

/// The values of a picking created through the ORM.
fn picking_vals(f: &Fixture, company: i64, picking_type: i64, name: &str) -> RawVals {
    RawVals::new()
        .set_cell("name", text(name))
        .set_cell("company_id", Cell::Id(company))
        .set_cell("picking_type_id", Cell::Id(picking_type))
        .set_cell("location_id", Cell::Id(f.location))
        .set_cell("location_dest_id", Cell::Id(f.other_location))
}

async fn create_picking(tx: &mut Transaction, company: i64, vals: RawVals) -> Result<i64> {
    tx.savepoint(async |tx| {
        Ok(tx
            .env()
            .with_company(company)?
            .records("stock.picking")?
            .create(tx, vec![vals])
            .await?
            .rec_ids()[0]
            .get())
    })
    .await
}

/// The values of a move of `company` through the ORM, with consistent product-unit columns (factor
/// one, Product Unit digits 2, zero quantities).
fn move_vals(f: &Fixture, company: i64) -> RawVals {
    RawVals::new()
        .set_cell("company_id", Cell::Id(company))
        .set_cell("product_id", Cell::Id(f.product))
        .set_cell("uom_id", Cell::Id(f.unit))
        .set_cell("location_id", Cell::Id(f.location))
        .set_cell("location_dest_id", Cell::Id(f.other_location))
        .set_cell("product_qty", dec("0"))
        .set_cell("quantity_product_uom", dec("0"))
        .set_cell("uom_factor", dec("1"))
        .set_cell("product_uom_factor", dec("1"))
        .set_cell("product_unit_digits", Cell::Int(2))
}

async fn create_move(tx: &mut Transaction, company: i64, vals: RawVals) -> Result<i64> {
    tx.savepoint(async |tx| {
        Ok(tx
            .env()
            .with_company(company)?
            .records("stock.move")?
            .create(tx, vec![vals])
            .await?
            .rec_ids()[0]
            .get())
    })
    .await
}

/// One move row written with plain SQL.
#[derive(Clone)]
struct MoveRow {
    demand: String,
    quantity: String,
    product_qty: String,
    quantity_product_uom: String,
    uom_factor: String,
    product_uom_factor: String,
    digits: i32,
    version: i32,
}

impl MoveRow {
    /// A consistent zero row: factors one, digits 2, version 0.
    fn zero() -> Self {
        Self {
            demand: "0".into(),
            quantity: "0".into(),
            product_qty: "0".into(),
            quantity_product_uom: "0".into(),
            uom_factor: "1".into(),
            product_uom_factor: "1".into(),
            digits: 2,
            version: 0,
        }
    }
}

const MOVE_INSERT: &str = "INSERT INTO stock_move \
    (company_id, product_id, uom_id, location_id, location_dest_id, date, state, procure_method, \
     open_line_version, product_uom_qty, quantity, product_qty, quantity_product_uom, uom_factor, \
     product_uom_factor, product_unit_digits, is_scrap, picked) \
    VALUES ($1, $2, $3, $4, $5, now(), 'draft', 'make_to_stock', $6, $7::text::numeric, \
     $8::text::numeric, $9::text::numeric, $10::text::numeric, $11::text::numeric, \
     $12::text::numeric, $13, false, false)";

async fn insert_move(f: &Fixture, tx: &mut Transaction, row: &MoveRow) -> Result<u64> {
    tx.savepoint(async |tx| {
        tx.execute(
            MOVE_INSERT,
            &[
                &f.company,
                &f.product,
                &f.unit,
                &f.location,
                &f.other_location,
                &row.version,
                &row.demand,
                &row.quantity,
                &row.product_qty,
                &row.quantity_product_uom,
                &row.uom_factor,
                &row.product_uom_factor,
                &row.digits,
            ],
        )
        .await
    })
    .await
}

fn ids(cell: Cell) -> Vec<i64> {
    let Cell::Ids(ids) = cell else {
        panic!("an id list, got {cell:?}")
    };
    let mut out: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    out.sort_unstable();
    out
}

fn rec_ids(ids: &[i64]) -> Vec<RecId> {
    ids.iter()
        .map(|id| RecId::from_real(*id).unwrap())
        .collect()
}

/// The reference keeps its supplied name, the name is required and readonly, and the table has the
/// stored relation tables of both of its many2many fields.
#[tokio::test]
async fn reference_name_and_relation_tables() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    assert_eq!(
        columns(&mut tx, "stock_reference", &["name"]).await,
        expect(&[("name", "character varying", false)])
    );
    let spec = f.h.registry().model("stock.reference").unwrap();
    let name = spec.field("name").unwrap();
    assert!(name.required && name.readonly && name.store);
    let id = create(
        &mut tx,
        "stock.reference",
        RawVals::new().set_cell("name", text("SO/0042")),
    )
    .await;
    let stored: String = tx
        .query("SELECT name FROM stock_reference WHERE id = $1", &[&id])
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(stored, "SO/0042");
    // without a name: the not-null refusal naming the field
    let error = tx
        .savepoint(async |tx| {
            tx.env()
                .records("stock.reference")?
                .create(tx, vec![RawVals::new()])
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert_eq!(
        error.message(),
        format!(
            "{PREFIX}Missing required field '{}' (name) for model '{}' (stock.reference)",
            name.string, spec.description
        )
    );
    for (table, a, b) in [
        ("stock_reference_move_rel", "reference_id", "move_id"),
        ("stock_reference_picking_rel", "reference_id", "picking_id"),
        ("stock_move_move_rel", "move_orig_id", "move_dest_id"),
        ("stock_route_move", "move_id", "route_id"),
        (
            "template_attribute_value_stock_move_rel",
            "move_id",
            "template_attribute_value_id",
        ),
    ] {
        assert_relation(&mut tx, table, a, b).await;
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// One reference relates to several moves, and the association reads the same from both sides (the
/// reference's `move_ids` and each move's `reference_ids` share `stock_reference_move_rel`); the
/// stored transfer side shares `stock_reference_picking_rel` with the picking's `reference_ids` (rebuilt
/// from the moves of the transfer; the transfer side itself is never written).
#[tokio::test]
async fn reference_relates_to_several_moves_from_both_sides() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "WH/IN/00001"),
    )
    .await
    .unwrap();
    let mut moves = Vec::new();
    for _ in 0..3 {
        moves.push(
            create_move(
                &mut tx,
                f.company,
                move_vals(&f, f.company).set_cell("picking_id", Cell::Id(picking)),
            )
            .await
            .unwrap(),
        );
    }
    let other = create_move(&mut tx, f.company, move_vals(&f, f.company))
        .await
        .unwrap();
    let reference = create(
        &mut tx,
        "stock.reference",
        RawVals::new()
            .set_cell("name", text("PO/0007"))
            .set_commands("move_ids", [Command::set(rec_ids(&moves))]),
    )
    .await;
    let references = tx.env().records("stock.reference").unwrap();
    let linked = ids(references
        .browse([reference])
        .unwrap()
        .cell(&mut tx, "move_ids")
        .await
        .unwrap());
    let mut expected = moves.clone();
    expected.sort_unstable();
    assert_eq!(linked, expected);
    let move_records = tx.env().records("stock.move").unwrap();
    for id in &moves {
        let back = ids(move_records
            .browse([*id])
            .unwrap()
            .cell(&mut tx, "reference_ids")
            .await
            .unwrap());
        assert_eq!(back, [reference], "move {id}");
    }
    let unlinked = ids(move_records
        .browse([other])
        .unwrap()
        .cell(&mut tx, "reference_ids")
        .await
        .unwrap());
    assert!(unlinked.is_empty());
    // the transfer side, from the picking
    let back = ids(tx
        .env()
        .records("stock.picking")
        .unwrap()
        .browse([picking])
        .unwrap()
        .cell(&mut tx, "reference_ids")
        .await
        .unwrap());
    assert_eq!(back, [reference]);
    // a second reference on one move: the association is many-to-many
    let second = create(
        &mut tx,
        "stock.reference",
        RawVals::new()
            .set_cell("name", text("SO/0008"))
            .set_commands("move_ids", [Command::set(rec_ids(&moves[..1]))]),
    )
    .await;
    let back = ids(move_records
        .browse([moves[0]])
        .unwrap()
        .cell(&mut tx, "reference_ids")
        .await
        .unwrap());
    let mut both = vec![reference, second];
    both.sort_unstable();
    assert_eq!(back, both);
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The move chain is one self relation read from both sides: a move's destinations are the moves
/// whose origins include it.
#[tokio::test]
async fn move_chain_reads_from_both_sides() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let first = create_move(&mut tx, f.company, move_vals(&f, f.company))
        .await
        .unwrap();
    let second = create_move(
        &mut tx,
        f.company,
        move_vals(&f, f.company).set_commands("move_orig_ids", [Command::set(rec_ids(&[first]))]),
    )
    .await
    .unwrap();
    let moves = tx.env().records("stock.move").unwrap();
    let dest = ids(moves
        .browse([first])
        .unwrap()
        .cell(&mut tx, "move_dest_ids")
        .await
        .unwrap());
    assert_eq!(dest, [second]);
    let row = tx
        .query(
            "SELECT move_orig_id, move_dest_id FROM stock_move_move_rel",
            &[],
        )
        .await
        .unwrap();
    let got: Vec<(i64, i64)> = row.iter().map(|r| (r.get(0), r.get(1))).collect();
    assert_eq!(got, [(first, second)]);
    let spec = f.h.registry().model("stock.move").unwrap();
    for field in ["move_dest_ids", "move_orig_ids", "price_unit"] {
        assert!(!spec.field(field).unwrap().copy, "{field}");
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

const COUNTERS: [&str; 14] = [
    "move_count",
    "draft_count",
    "waiting_count",
    "confirmed_count",
    "partially_available_count",
    "assigned_count",
    "done_count",
    "cancel_count",
    "done_scrap_count",
    "cancel_scrap_count",
    "assigned_zero_demand_count",
    "open_positive_demand_count",
    "confirmed_positive_demand_count",
    "mto_nonscrap_count",
];

/// The picking columns, its ORM defaults (no name default), the counter vector at zero, the readonly
/// flags and its indexes, the partial one with the plan's predicate.
#[tokio::test]
async fn picking_columns_defaults_and_indexes() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let mut spec_cols = vec![
        ("name", "character varying", false),
        ("origin", "character varying", true),
        ("note", "text", true),
        ("backorder_id", "bigint", true),
        ("return_id", "bigint", true),
        ("move_type", "character varying", false),
        ("state", "character varying", false),
        ("priority", "character varying", true),
        ("scheduled_date", "timestamp without time zone", true),
        ("date_deadline", "timestamp without time zone", true),
        ("has_deadline_issue", "boolean", true),
        ("date_done", "timestamp without time zone", true),
        ("location_id", "bigint", false),
        ("location_dest_id", "bigint", false),
        ("picking_type_id", "bigint", false),
        ("partner_id", "bigint", true),
        ("owner_id", "bigint", true),
        ("company_id", "bigint", false),
        ("user_id", "bigint", true),
        ("printed", "boolean", true),
        ("is_locked", "boolean", true),
        ("shipping_weight", "numeric", true),
        ("show_return", "boolean", true),
    ];
    for counter in COUNTERS {
        spec_cols.push((counter, "integer", false));
    }
    let names: Vec<&str> = spec_cols.iter().map(|c| c.0).collect();
    assert_eq!(
        columns(&mut tx, "stock_picking", &names).await,
        expect(&spec_cols)
    );
    let spec = f.h.registry().model("stock.picking").unwrap();
    for field in COUNTERS.iter().copied().chain([
        "state",
        "backorder_id",
        "return_id",
        "date_done",
        "reference_ids",
    ]) {
        assert!(spec.field(field).unwrap().readonly, "{field} is readonly");
    }
    // a picking without a name takes its type's next reference; a type without numbering refuses
    // it by name (the "/" placeholder would only meet the per-company reference key)
    let error = create_picking(
        &mut tx,
        f.company,
        RawVals::new()
            .set_cell("company_id", Cell::Id(f.company))
            .set_cell("picking_type_id", Cell::Id(f.picking_type))
            .set_cell("location_id", Cell::Id(f.location))
            .set_cell("location_dest_id", Cell::Id(f.other_location)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert!(error.message().contains("Sequence Prefix"), "{error:?}");
    let id = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "WH/IN/00001"),
    )
    .await
    .unwrap();
    let row = tx
        .query(
            "SELECT state, move_type, priority, is_locked::text, show_return::text, \
             (scheduled_date IS NOT NULL)::text, user_id::text FROM stock_picking WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap();
    let got: Vec<String> = (0..7).map(|i| row[0].get(i)).collect();
    let uid = tx.env().uid().to_string();
    assert_eq!(
        got,
        ["draft", "direct", "0", "true", "true", "true", uid.as_str()]
    );
    for counter in COUNTERS {
        let value: i32 = tx
            .query(
                &format!("SELECT {counter} FROM stock_picking WHERE id = $1"),
                &[&id],
            )
            .await
            .unwrap()[0]
            .get(0);
        assert_eq!(value, 0, "{counter}");
    }
    for field in ["name", "origin"] {
        assert_trigram(&mut tx, "stock_picking", field).await;
    }
    for (field, not_null) in [
        ("backorder_id", true),
        ("return_id", true),
        ("partner_id", true),
        ("owner_id", true),
        ("user_id", true),
        ("scheduled_date", false),
        ("picking_type_id", false),
        ("location_id", false),
        ("location_dest_id", false),
    ] {
        assert_btree(&mut tx, "stock_picking", field, not_null).await;
    }
    // the company leads the reference key, so it has no index of its own (N1)
    let own_company_index: i64 = tx
        .query(
            "SELECT count(*) FROM pg_indexes WHERE schemaname = current_schema() \
             AND tablename = 'stock_picking' AND indexname = 'stock_picking__company_id_index'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(own_company_index, 0);
    let probe = probe_definition(
        &mut tx,
        "stock_picking",
        "(picking_type_id, state) WHERE state NOT IN ('done', 'cancel')",
    )
    .await;
    assert_eq!(
        index_definition(&mut tx, "stock_picking_open_type_state").await,
        probe
    );
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// A second reference in one company is refused whatever its operation type; the same reference in
/// another company is allowed.
#[tokio::test]
async fn picking_reference_is_unique_per_company() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let message = "Another transfer of this company already has this reference.";
    create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "REF"),
    )
    .await
    .unwrap();
    let error = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "REF"),
    )
    .await
    .unwrap_err();
    assert_violation(&error, "23505", "stock_picking_name_uniq", message);
    // another operation type of the same company: still refused
    let delivery = create_picking_type(
        &mut tx,
        f.company,
        "Delivery Orders",
        f.location,
        f.other_location,
    )
    .await;
    let error = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, delivery, "REF"),
    )
    .await
    .unwrap_err();
    assert_violation(&error, "23505", "stock_picking_name_uniq", message);
    // the other company, with its own type: allowed
    let other = create_picking(
        &mut tx,
        f.other_company,
        picking_vals(&f, f.other_company, f.other_picking_type, "REF"),
    )
    .await
    .unwrap();
    let rows = tx
        .query(
            "SELECT company_id FROM stock_picking WHERE name = 'REF' ORDER BY id",
            &[],
        )
        .await
        .unwrap();
    let got: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
    assert_eq!(got, [f.company, f.other_company]);
    assert!(other > 0);
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// Set the named counters of picking `id` (the others stay as they are) in a savepoint.
async fn set_counters(tx: &mut Transaction, id: i64, values: &[(&str, i32)]) -> Result<u64> {
    let sets: Vec<String> = values.iter().map(|(c, v)| format!("{c} = {v}")).collect();
    let sql = format!("UPDATE stock_picking SET {} WHERE id = $1", sets.join(", "));
    tx.savepoint(async |tx| tx.execute(&sql, &[&id]).await)
        .await
}

/// The counter vector's checks (3): no count is negative, the seven state counts add up
/// to the move count, and each subset count stays within its set; the boundaries are accepted.
#[tokio::test]
async fn picking_counter_vector_checks() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let id = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "WH/IN/00001"),
    )
    .await
    .unwrap();
    // every counter refuses -1 (the others at zero)
    for counter in COUNTERS {
        let error = set_counters(&mut tx, id, &[(counter, -1)])
            .await
            .unwrap_err();
        assert_violation(&error, "23514", "stock_picking_counts_nonneg", PICK_NONNEG);
    }
    // a state count without its move, and a move without a state
    for values in [
        &[("draft_count", 1)][..],
        &[("move_count", 1)][..],
        &[("move_count", 2), ("done_count", 1)][..],
    ] {
        let error = set_counters(&mut tx, id, values).await.unwrap_err();
        assert_violation(&error, "23514", "stock_picking_counts_sum", PICK_SUM);
    }
    // each subset beyond its set (the sum holds)
    let one_each: Vec<(&str, i32)> = vec![
        ("move_count", 7),
        ("draft_count", 1),
        ("waiting_count", 1),
        ("confirmed_count", 1),
        ("partially_available_count", 1),
        ("assigned_count", 1),
        ("done_count", 1),
        ("cancel_count", 1),
    ];
    set_counters(&mut tx, id, &one_each).await.unwrap();
    // open moves: 7 - done 1 - cancel 1 = 5; moves not known to be scrap once both scrap counts
    // are at their boundary 1: 7 - 1 - 1 = 5. Open positive comes before confirmed positive,
    // which it bounds.
    for (counter, over) in [
        ("done_scrap_count", 2),
        ("cancel_scrap_count", 2),
        ("assigned_zero_demand_count", 2),
        ("open_positive_demand_count", 6),
        ("confirmed_positive_demand_count", 2),
        ("mto_nonscrap_count", 6),
    ] {
        let error = set_counters(&mut tx, id, &[(counter, over)])
            .await
            .unwrap_err();
        assert_violation(
            &error,
            "23514",
            "stock_picking_counts_subsets",
            PICK_SUBSETS,
        );
        // the boundary: equal to its set
        set_counters(&mut tx, id, &[(counter, over - 1)])
            .await
            .unwrap_or_else(|e| panic!("{counter} = {}: {e:?}", over - 1));
    }
    // confirmed positive (1) is within open positive: open positive 0 is refused
    let error = set_counters(&mut tx, id, &[("open_positive_demand_count", 0)])
        .await
        .unwrap_err();
    assert_violation(
        &error,
        "23514",
        "stock_picking_counts_subsets",
        PICK_SUBSETS,
    );
    // the ORM path never reaches the checks: the vector follows from the moves
    let error = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "WH/IN/00002")
            .set_cell("move_count", Cell::Int(1)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming, "{error:?}");
    // the shipping weight is a finite number; a finite value is accepted
    for value in ["NaN", "Infinity", "-Infinity"] {
        let sql =
            format!("UPDATE stock_picking SET shipping_weight = '{value}'::numeric WHERE id = $1");
        let error = tx
            .savepoint(async |tx| tx.execute(&sql, &[&id]).await)
            .await
            .unwrap_err();
        assert_violation(
            &error,
            "23514",
            "stock_picking_finite_shipping_weight",
            "The shipping weight of a transfer must be a finite number.",
        );
    }
    tx.savepoint(async |tx| {
        tx.execute(
            "UPDATE stock_picking SET shipping_weight = 12.5 WHERE id = $1",
            &[&id],
        )
        .await
    })
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The move columns, its ORM defaults, readonly flags and indexes, the partial one with the plan's
/// predicate and no plain index on `state`.
#[tokio::test]
async fn move_columns_defaults_and_indexes() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let spec_cols = [
        ("sequence", "integer", true),
        ("priority", "character varying", true),
        ("date", "timestamp without time zone", false),
        ("date_deadline", "timestamp without time zone", true),
        ("company_id", "bigint", false),
        ("product_id", "bigint", false),
        ("description_picking_manual", "text", true),
        ("product_uom_qty", "numeric", false),
        ("uom_id", "bigint", false),
        ("location_id", "bigint", false),
        ("location_dest_id", "bigint", false),
        ("forecasted_location_id", "bigint", true),
        ("partner_id", "bigint", true),
        ("picking_id", "bigint", true),
        ("state", "character varying", false),
        ("picked", "boolean", false),
        ("price_unit", "numeric", true),
        ("origin", "character varying", true),
        ("procure_method", "character varying", false),
        ("is_scrap", "boolean", false),
        ("rule_id", "bigint", true),
        ("propagate_cancel", "boolean", true),
        ("picking_type_id", "bigint", true),
        ("is_inventory", "boolean", true),
        ("inventory_name", "character varying", true),
        ("origin_returned_move_id", "bigint", true),
        ("restrict_partner_id", "bigint", true),
        ("warehouse_id", "bigint", true),
        ("quantity", "numeric", false),
        ("additional", "boolean", true),
        ("reference", "character varying", true),
        ("next_serial", "character varying", true),
        ("next_serial_count", "integer", true),
        ("reservation_date", "date", true),
        ("packaging_uom_id", "bigint", true),
        ("packaging_uom_qty", "numeric", true),
        ("should_replenish_scrapped", "boolean", true),
        ("open_line_version", "integer", false),
        ("done_at", "timestamp without time zone", true),
        ("product_qty", "numeric", false),
        ("quantity_product_uom", "numeric", false),
        ("uom_factor", "numeric", false),
        ("product_uom_factor", "numeric", false),
        ("product_unit_digits", "integer", false),
    ];
    let names: Vec<&str> = spec_cols.iter().map(|c| c.0).collect();
    assert_eq!(
        columns(&mut tx, "stock_move", &names).await,
        expect(&spec_cols)
    );
    let spec = f.h.registry().model("stock.move").unwrap();
    for field in [
        "state",
        "date_deadline",
        "description_picking_manual",
        "is_scrap",
        "inventory_name",
        "quantity",
        "reference",
        "open_line_version",
        "done_at",
        "product_qty",
        "quantity_product_uom",
        "uom_factor",
        "product_uom_factor",
        "product_unit_digits",
    ] {
        assert!(spec.field(field).unwrap().readonly, "{field} is readonly");
    }
    // the product-unit quantities have no default: the writer supplies them
    for field in ["product_qty", "quantity_product_uom", "done_at"] {
        assert!(spec.field(field).unwrap().default.is_none(), "{field}");
    }
    let id = create_move(&mut tx, f.company, move_vals(&f, f.company))
        .await
        .unwrap();
    let row = tx
        .query(
            "SELECT sequence::text, state, procure_method, propagate_cancel::text, \
             open_line_version::text, quantity::text, product_uom_qty::text, priority, picked::text, \
             additional::text, (done_at IS NULL)::text FROM stock_move WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap();
    let got: Vec<String> = (0..11).map(|i| row[0].get(i)).collect();
    assert_eq!(
        got,
        [
            "10",
            "draft",
            "make_to_stock",
            "true",
            "0",
            "0",
            "0",
            "0",
            "false",
            "false",
            "true"
        ]
    );
    for (field, not_null) in [
        ("date", false),
        ("company_id", false),
        ("product_id", false),
        ("location_id", false),
        ("location_dest_id", false),
        ("forecasted_location_id", false),
        ("picking_id", false),
        ("origin_returned_move_id", false),
        ("partner_id", true),
        ("picking_type_id", true),
        ("restrict_partner_id", true),
        ("rule_id", true),
        ("warehouse_id", true),
        ("packaging_uom_id", true),
        ("uom_id", false),
    ] {
        assert_btree(&mut tx, "stock_move", field, not_null).await;
    }
    let probe = probe_definition(
        &mut tx,
        "stock_move",
        "(product_id, location_id, location_dest_id, company_id) WHERE state NOT IN ('done', 'cancel')",
    )
    .await;
    assert_eq!(
        index_definition(&mut tx, "stock_move_open_product_location").await,
        probe
    );
    // key list exactly `(state)`; the partial index's predicate also reads `((state)::text ...)`
    let state_indexes: i64 = tx
        .query(
            "SELECT count(*) FROM pg_indexes WHERE schemaname = current_schema() \
             AND tablename = 'stock_move' AND indexdef ~ 'USING \\w+ \\(state\\)'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(state_indexes, 0, "no plain index on stock_move.state");
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The move's checks refuse each violation with their own constraint (the finite, factor and digits
/// checks sort before the conversion checks, so a NaN, a zero factor or bad digits is reported as
/// such, never as a division error), and accept the boundaries.
#[tokio::test]
async fn move_checks_refuse_and_accept_boundaries() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let row = MoveRow::zero;
    let mut cases: Vec<(MoveRow, &str)> = Vec::new();
    for bad in ["NaN", "Infinity", "-Infinity"] {
        cases.push((
            MoveRow {
                demand: bad.into(),
                ..row()
            },
            "stock_move_finite_product_uom_qty",
        ));
        cases.push((
            MoveRow {
                quantity: bad.into(),
                ..row()
            },
            "stock_move_finite_quantity",
        ));
        cases.push((
            MoveRow {
                product_qty: bad.into(),
                ..row()
            },
            "stock_move_finite_product_qty",
        ));
        cases.push((
            MoveRow {
                quantity_product_uom: bad.into(),
                ..row()
            },
            "stock_move_finite_quantity_product_uom",
        ));
    }
    // a NaN or infinite factor is nonzero for PostgreSQL; the finite check reports it
    for bad in ["NaN", "Infinity", "-Infinity"] {
        cases.push((
            MoveRow {
                uom_factor: bad.into(),
                ..row()
            },
            "stock_move_finite_uom_factor",
        ));
        cases.push((
            MoveRow {
                product_uom_factor: bad.into(),
                ..row()
            },
            "stock_move_finite_product_uom_factor",
        ));
    }
    // a zero factor is reported by the factor check (23514), never as a division by zero (22012)
    for bad in ["0", "0.000"] {
        cases.push((
            MoveRow {
                uom_factor: bad.into(),
                ..row()
            },
            "stock_move_factor_nonzero",
        ));
        cases.push((
            MoveRow {
                product_uom_factor: bad.into(),
                ..row()
            },
            "stock_move_factor_nonzero",
        ));
    }
    for bad in [-1, 29] {
        cases.push((
            MoveRow {
                digits: bad,
                ..row()
            },
            "stock_move_digits_range",
        ));
    }
    cases.push((
        MoveRow {
            version: -1,
            ..row()
        },
        "stock_move_open_line_version_nonneg",
    ));
    // 1 unit of factor 3 is 3 product units of factor 1
    cases.push((
        MoveRow {
            demand: "1".into(),
            uom_factor: "3".into(),
            quantity_product_uom: "0".into(),
            ..row()
        },
        "stock_move_product_qty_conversion",
    ));
    cases.push((
        MoveRow {
            quantity: "1".into(),
            uom_factor: "3".into(),
            demand: "1".into(),
            product_qty: "3".into(),
            ..row()
        },
        "stock_move_quantity_product_uom_conversion",
    ));
    for (case, constraint) in &cases {
        let error = insert_move(&f, &mut tx, case).await.unwrap_err();
        assert_violation(&error, "23514", constraint, move_message(constraint));
    }
    // boundaries: digits 0 and 28, version 0, a tiny factor, negative quantities and factors (a
    // stored unit may have a negative factor)
    let accepted = [
        MoveRow { digits: 0, ..row() },
        MoveRow {
            digits: 28,
            ..row()
        },
        MoveRow {
            demand: "-2".into(),
            quantity: "-2".into(),
            product_qty: "-6".into(),
            quantity_product_uom: "-6".into(),
            uom_factor: "3".into(),
            ..row()
        },
        MoveRow {
            demand: "1".into(),
            quantity: "0".into(),
            product_qty: "0.00".into(),
            quantity_product_uom: "0".into(),
            uom_factor: "0.000000001".into(),
            ..row()
        },
        MoveRow {
            demand: "2".into(),
            quantity: "2".into(),
            product_qty: "-6".into(),
            quantity_product_uom: "-6".into(),
            uom_factor: "-3".into(),
            ..row()
        },
        MoveRow {
            demand: "2".into(),
            quantity: "2".into(),
            product_qty: "6".into(),
            quantity_product_uom: "6".into(),
            uom_factor: "-3".into(),
            product_uom_factor: "-1".into(),
            ..row()
        },
    ];
    for case in &accepted {
        assert_eq!(insert_move(&f, &mut tx, case).await.unwrap(), 1);
    }
    // the ORM path never reaches the conversion check: the unit service fills the converted demand
    // and refuses a supplied value that differs, by name, before the insert
    let error = create_move(
        &mut tx,
        f.company,
        move_vals(&f, f.company)
            .set_cell("product_uom_qty", dec("1"))
            .set_cell("product_qty", dec("2")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.sqlstate(), None, "{error:?}");
    assert_eq!(
        error.message(),
        erp_addon_stock::StockError::UnitSnapshot {
            field: tx
                .registry()
                .model("stock.move")
                .unwrap()
                .field("product_qty")
                .unwrap()
                .string
                .to_string()
        }
        .to_string()
    );
    // unit price and packaging quantity are finite numbers; finite values are accepted
    insert_move(&f, &mut tx, &MoveRow::zero()).await.unwrap();
    let last: i64 = tx
        .query("SELECT max(id) FROM stock_move", &[])
        .await
        .unwrap()[0]
        .get(0);
    for (column, constraint) in [
        ("price_unit", "stock_move_finite_price_unit"),
        ("packaging_uom_qty", "stock_move_finite_packaging_uom_qty"),
    ] {
        for value in ["NaN", "Infinity", "-Infinity"] {
            let sql = format!("UPDATE stock_move SET {column} = '{value}'::numeric WHERE id = $1");
            let error = tx
                .savepoint(async |tx| tx.execute(&sql, &[&last]).await)
                .await
                .unwrap_err();
            assert_violation(&error, "23514", constraint, move_message(constraint));
        }
        let sql = format!("UPDATE stock_move SET {column} = 3.75 WHERE id = $1");
        tx.savepoint(async |tx| tx.execute(&sql, &[&last]).await)
            .await
            .unwrap();
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The two conversion checks are, as declared on the model, exactly the unit service's SQL form of
/// the conversion (`product_quantity_sql`) over the move's own columns, so the database and the
/// service compute one expression and the naive `round(qty * factor / factor, digits)`, which
/// double-rounds near ties (`addons/uom/tests/stock_snapshot.rs:380-388`), is not used. The declared
/// text is read from the registry, not from the catalog's deparsed form.
#[tokio::test]
async fn conversion_checks_are_the_unit_service_expression() {
    let f = fixture().await;
    let spec = f.h.registry().model("stock.move").unwrap();
    for (constraint, column, qty) in [
        (
            "stock_move_product_qty_conversion",
            "product_qty",
            "product_uom_qty",
        ),
        (
            "stock_move_quantity_product_uom_conversion",
            "quantity_product_uom",
            "quantity",
        ),
    ] {
        let expr = product_quantity_sql(
            qty,
            "uom_factor",
            "product_uom_factor",
            "product_unit_digits",
        )
        .unwrap();
        let declared = match spec
            .table_object(constraint)
            .unwrap_or_else(|| panic!("{constraint} is declared"))
            .kind
        {
            erp_sdk::__private::TableObjectKind::Constraint { definition, .. } => definition,
            ref other => panic!("{constraint} is a CHECK constraint, got {other:?}"),
        };
        assert_eq!(
            declared,
            format!("CHECK ({column} = {expr})"),
            "{constraint}"
        );
    }
    f.h.cleanup().await.unwrap();
}

/// One conversion of the C3 corpus.
struct Conversion {
    qty: String,
    move_factor: String,
    product_factor: String,
    digits: u32,
}

/// The corpus: ratios with a finite decimal form (3, 12, 0.125, 10^6) and without one (1/3, 1/12 and 7
/// written as long decimals), large factors, negative quantities, values on and near HALF-UP ties, and
/// the near-tie shape of the unit service's own corpus (`addons/uom/tests/stock_snapshot.rs`): a ratio
/// `5 * 10^-(digits+1)` off by one in the last place of a long product factor.
fn corpus() -> Vec<Conversion> {
    let mut rows = Vec::new();
    let factors = [
        "1",
        "3",
        "-3",
        "7",
        "12",
        "0.125",
        "0.3333333333",
        "0.08333333333333",
        "1000000",
        "0.000001",
        "14.285714285714285714",
    ];
    let quantities = [
        "1", "2.5", "-1", "-2.5", "0.005", "-0.005", "0.015", "10", "123.456", "1000000", "0.0001",
        "7", "3", "0.125", "-0.375",
    ];
    for move_factor in factors {
        for product_factor in factors {
            for qty in quantities {
                for digits in [0, 2, 6] {
                    rows.push(Conversion {
                        qty: qty.to_owned(),
                        move_factor: move_factor.to_owned(),
                        product_factor: product_factor.to_owned(),
                        digits,
                    });
                }
            }
        }
    }
    for digits in 0..=6u32 {
        for a in [16u32, 18, 20] {
            let source = 5 * 10i128.pow(a);
            let power = 10i128.pow(a + digits + 1);
            for target in [power + 1, power - 1] {
                for qty in ["1", "-1"] {
                    rows.push(Conversion {
                        qty: qty.to_owned(),
                        move_factor: source.to_string(),
                        product_factor: target.to_string(),
                        digits,
                    });
                }
            }
        }
    }
    rows
}

/// C3 on the fallback shape: for every corpus row, the value of the unit service
/// (`ProductUnitPreview::product_quantity`: exact `qty * move factor / product factor`, HALF-UP at
/// the digits) is written as `product_qty` and `quantity_product_uom`, and the move's two conversion
/// checks must accept it. Every refused row is listed, with the naive `round(q * f / f, d)` value
/// beside it for comparison only; none is adjusted or dropped. No spec tag: asks for generated
/// columns, which this slice does not ship.
#[tokio::test]
async fn product_unit_check_agrees_with_the_unit_service() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let rows = corpus();
    let mut differences = Vec::new();
    for c in &rows {
        let preview = ProductUnitPreview::new(
            &Unit::new(1, 1, Decimal::from_str_exact(&c.move_factor).unwrap()).unwrap(),
            &Unit::new(2, 1, Decimal::from_str_exact(&c.product_factor).unwrap()).unwrap(),
            c.digits,
        )
        .unwrap();
        let expected = preview
            .product_quantity(Decimal::from_str_exact(&c.qty).unwrap())
            .unwrap()
            .to_string();
        let digits = i32::try_from(c.digits).unwrap();
        let row = MoveRow {
            demand: c.qty.clone(),
            quantity: c.qty.clone(),
            product_qty: expected.clone(),
            quantity_product_uom: expected.clone(),
            uom_factor: c.move_factor.clone(),
            product_uom_factor: c.product_factor.clone(),
            digits,
            version: 0,
        };
        if let Err(error) = insert_move(&f, &mut tx, &row).await {
            let pg: String = tx
                .query(
                    "SELECT round($1::text::numeric * $2::text::numeric / $3::text::numeric, $4)::text",
                    &[&c.qty, &c.move_factor, &c.product_factor, &digits],
                )
                .await
                .unwrap()[0]
                .get(0);
            differences.push(format!(
                "qty {} * {} / {} at {} digits: service {expected}, PostgreSQL {pg} ({:?})",
                c.qty,
                c.move_factor,
                c.product_factor,
                c.digits,
                error.constraint()
            ));
        }
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
    assert!(
        differences.is_empty(),
        "the conversion check refuses the unit service's value on {} of {} rows:\n{}",
        differences.len(),
        rows.len(),
        differences.join("\n")
    );
}

/// The variant of a new product template restricted to `company`.
async fn restricted_product(tx: &mut Transaction, company: i64, name: &str) -> i64 {
    let env = tx.env().with_company(company).unwrap();
    let template = env
        .records("product.template")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text(name))
                    .set_cell("company_id", Cell::Id(company)),
            ],
        )
        .await
        .unwrap()
        .rec_ids()[0]
        .get();
    tx.query(
        "SELECT id FROM product_product WHERE product_tmpl_id = $1",
        &[&template],
    )
    .await
    .unwrap()[0]
        .get(0)
}

/// Assert `error` is the company-crossover refusal naming `field` and its target.
fn assert_crossover(error: &Error, field: &str, label: &str, value: &str) {
    assert_eq!(error.kind(), ErrorKind::UserError, "{field}: {error:?}");
    let message = error.message();
    assert!(
        message.starts_with("These values link records of different companies:\n- “"),
        "{field}: {message}"
    );
    assert!(
        message.contains(&format!(
            "is in company “Main Company”, and “{label}” ({field}: '{value}') points to a record of another company.\n"
        )),
        "{field}: {message}"
    );
    assert!(
        message
            .ends_with("Choose records of the same company, or records shared by all companies."),
        "{field}: {message}"
    );
}

/// No spec case covers these links yet, so the test carries no tag.
#[tokio::test]
async fn picking_and_move_links_must_share_the_row_company() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let foreign_location = create(
        &mut tx,
        "stock.location",
        RawVals::new()
            .set_cell("name", text("Shelf B"))
            .set_cell("company_id", Cell::Id(f.other_company)),
    )
    .await;
    let foreign_partner = create(
        &mut tx,
        "res.partner",
        RawVals::new()
            .set_cell("name", text("Foreign Partner"))
            .set_cell("company_id", Cell::Id(f.other_company)),
    )
    .await;
    let foreign_picking = create_picking(
        &mut tx,
        f.other_company,
        picking_vals(&f, f.other_company, f.other_picking_type, "B/IN/00001"),
    )
    .await
    .unwrap();
    let foreign_move = create_move(
        &mut tx,
        f.other_company,
        move_vals(&f, f.other_company).set_cell("reference", text("B/IN/00001")),
    )
    .await
    .unwrap();
    let foreign_product = restricted_product(&mut tx, f.other_company, "Foreign Widget").await;
    let spec = f.h.registry().model("stock.picking").unwrap();
    let label = |field: &str| spec.field(field).unwrap().string.clone();
    for (field, target, value) in [
        ("picking_type_id", f.other_picking_type, "Receipts B"),
        ("location_id", foreign_location, "Shelf B"),
        ("location_dest_id", foreign_location, "Shelf B"),
        ("partner_id", foreign_partner, "Foreign Partner"),
        ("owner_id", foreign_partner, "Foreign Partner"),
        ("backorder_id", foreign_picking, "B/IN/00001"),
        ("return_id", foreign_picking, "B/IN/00001"),
    ] {
        let error = create_picking(
            &mut tx,
            f.company,
            picking_vals(&f, f.company, f.picking_type, "WH/IN/X")
                .set_cell(field, Cell::Id(target)),
        )
        .await
        .unwrap_err();
        assert_crossover(&error, field, &label(field), value);
    }
    let move_spec = f.h.registry().model("stock.move").unwrap();
    let move_label = |field: &str| move_spec.field(field).unwrap().string.clone();
    for (field, target, value) in [
        ("product_id", foreign_product, "Foreign Widget"),
        ("location_id", foreign_location, "Shelf B"),
        ("location_dest_id", foreign_location, "Shelf B"),
        ("forecasted_location_id", foreign_location, "Shelf B"),
        ("partner_id", foreign_partner, "Foreign Partner"),
        ("restrict_partner_id", foreign_partner, "Foreign Partner"),
        ("picking_id", foreign_picking, "B/IN/00001"),
        ("picking_type_id", f.other_picking_type, "Receipts B"),
        ("origin_returned_move_id", foreign_move, "B/IN/00001"),
    ] {
        let error = create_move(
            &mut tx,
            f.company,
            move_vals(&f, f.company).set_cell(field, Cell::Id(target)),
        )
        .await
        .unwrap_err();
        assert_crossover(&error, field, &move_label(field), value);
    }
    // shared targets and the row company's own type: accepted
    let own = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "WH/IN/00001")
            .set_cell("partner_id", Cell::Id(f.partner))
            .set_cell("owner_id", Cell::Id(f.partner)),
    )
    .await
    .unwrap();
    create_move(
        &mut tx,
        f.company,
        move_vals(&f, f.company)
            .set_cell("picking_id", Cell::Id(own))
            .set_cell("picking_type_id", Cell::Id(f.picking_type))
            .set_cell("partner_id", Cell::Id(f.partner))
            .set_cell("restrict_partner_id", Cell::Id(f.partner)),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn deleting_a_partner_who_owns_a_transfer_or_restricts_a_move_is_refused() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "OWN/1")
            .set_cell("owner_id", Cell::Id(f.partner)),
    )
    .await
    .unwrap();
    let blocked = |model: &str, description: &str| {
        format!(
            "{PREFIX}Other records still point to the record you want to delete.\n\nReferring model: '{description}' ({model})"
        )
    };
    let delete = async |tx: &mut Transaction| {
        tx.savepoint(async |tx| {
            tx.execute("DELETE FROM res_partner WHERE id = $1", &[&f.partner])
                .await
        })
        .await
    };
    let error = delete(&mut tx).await.unwrap_err();
    assert_eq!(error.sqlstate(), Some("23001"), "{error:?}");
    assert!(
        error
            .message()
            .starts_with(&blocked("stock.picking", "Transfer")),
        "{}",
        error.message()
    );
    let owner: Option<i64> = tx
        .query(
            "SELECT owner_id FROM stock_picking WHERE id = $1",
            &[&picking],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(owner, Some(f.partner));
    tx.execute(
        "UPDATE stock_picking SET owner_id = NULL WHERE id = $1",
        &[&picking],
    )
    .await
    .unwrap();
    let mv = create_move(
        &mut tx,
        f.company,
        move_vals(&f, f.company).set_cell("restrict_partner_id", Cell::Id(f.partner)),
    )
    .await
    .unwrap();
    let error = delete(&mut tx).await.unwrap_err();
    assert_eq!(error.sqlstate(), Some("23001"), "{error:?}");
    assert!(
        error
            .message()
            .starts_with(&blocked("stock.move", "Stock Move")),
        "{}",
        error.message()
    );
    tx.execute(
        "UPDATE stock_move SET restrict_partner_id = NULL WHERE id = $1",
        &[&mv],
    )
    .await
    .unwrap();
    assert_eq!(delete(&mut tx).await.unwrap(), 1);
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

// ---- the transfer's move counter vector and state (3) ----

/// `vals` with demand `qty` in both the move unit and the product unit (factor one, two digits).
fn with_demand(vals: RawVals, qty: &str) -> RawVals {
    vals.set_cell("product_uom_qty", dec(qty))
        .set_cell("product_qty", dec(qty))
}

/// One move of the fixture's company in `picking`: `state`, demand `qty`, scrap flag, supply
/// method.
fn counted_move(
    f: &Fixture,
    picking: i64,
    state: &str,
    qty: &str,
    scrap: bool,
    mto: bool,
) -> RawVals {
    with_demand(move_vals(f, f.company), qty)
        .set_cell("picking_id", Cell::Id(picking))
        .set_cell("state", text(state))
        .set_cell("is_scrap", Cell::Bool(scrap))
        .set_cell(
            "procure_method",
            text(if mto {
                "make_to_order"
            } else {
                "make_to_stock"
            }),
        )
}

/// Put `moves` in `state` the way the stock engine's statements do: one plain statement, then the
/// ORM is told of the changed moves and transfer counters. A move's status follows from its lines,
/// confirmation and reservation, so no create or write may name it; these tests need states those
/// paths do not reach yet (done, cancelled, waiting without origins).
async fn engine_state(tx: &mut Transaction, moves: &[i64], state: &str) {
    tx.flush().await.unwrap();
    let mut pickings: Vec<i64> = tx
        .query(
            "UPDATE stock_move SET state = $2::text, done_at = CASE WHEN $2::text = 'done' \
             THEN COALESCE(done_at, now() AT TIME ZONE 'UTC') ELSE done_at END \
             WHERE id = ANY($1) RETURNING picking_id",
            &[&moves, &state],
        )
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.get::<_, Option<i64>>(0))
        .collect();
    pickings.sort_unstable();
    pickings.dedup();
    let model = tx.env().records("stock.move").unwrap();
    model
        .browse(moves.iter().copied())
        .unwrap()
        .modified_external(tx, &["state"])
        .await
        .unwrap();
    erp_addon_stock::move_counters::notify_pickings(&model, tx, &pickings)
        .await
        .unwrap();
}

/// The `state` a test names for a move, taken out of its values for [`engine_state`].
fn take_state(vals: &mut RawVals) -> Option<String> {
    match vals.remove("state") {
        None => None,
        Some(Cell::Str(state)) => Some(state.to_string()),
        Some(other) => panic!("a move state is text, got {other:?}"),
    }
}

/// Moves created through the ORM; a named non-draft `state` is then set by [`engine_state`].
async fn create_moves(
    tx: &mut Transaction,
    company: i64,
    mut vals: Vec<RawVals>,
) -> Result<Vec<i64>> {
    let states: Vec<Option<String>> = vals.iter_mut().map(take_state).collect();
    let ids = tx
        .savepoint(async |tx| {
            Ok(tx
                .env()
                .with_company(company)?
                .records("stock.move")?
                .create(tx, vals)
                .await?
                .ids())
        })
        .await?;
    for (id, state) in ids.iter().zip(states) {
        if let Some(state) = state.filter(|state| state != "draft") {
            engine_state(tx, &[*id], &state).await;
        }
    }
    Ok(ids)
}

/// A move write through the ORM; a named `state` is then set by [`engine_state`].
async fn write_moves(
    tx: &mut Transaction,
    company: i64,
    ids: &[i64],
    mut vals: RawVals,
) -> Result<()> {
    let state = take_state(&mut vals);
    if !vals.is_empty() {
        tx.savepoint(async |tx| {
            tx.env()
                .with_company(company)?
                .records("stock.move")?
                .browse(ids.iter().copied())?
                .write(tx, vals)
                .await
        })
        .await?;
    }
    if let Some(state) = state {
        engine_state(tx, ids, &state).await;
    }
    Ok(())
}

async fn unlink_moves(tx: &mut Transaction, company: i64, ids: &[i64]) -> Result<()> {
    tx.savepoint(async |tx| {
        tx.env()
            .with_company(company)?
            .records("stock.move")?
            .browse(ids.iter().copied())?
            .unlink(tx)
            .await
    })
    .await
}

/// The stored vector of `picking` (in [`COUNTERS`] order) and its state.
async fn stored_vector(tx: &mut Transaction, picking: i64) -> (Vec<i64>, String) {
    let columns: Vec<String> = COUNTERS.iter().map(|c| format!("{c}::int8")).collect();
    let sql = format!(
        "SELECT {}, state::text FROM stock_picking WHERE id = $1",
        columns.join(", ")
    );
    let row = tx.query(&sql, &[&picking]).await.unwrap().remove(0);
    let vector = (0..COUNTERS.len()).map(|i| row.get::<_, i64>(i)).collect();
    (vector, row.get(COUNTERS.len()))
}

/// An independent fold of the moves of `picking` by the.3 definitions, in
/// [`COUNTERS`] order: open demand is non-zero demand, confirmed demand is positive demand.
async fn folded_vector(tx: &mut Transaction, picking: i64) -> Vec<i64> {
    let row = tx
        .query(
            "SELECT count(*)::int8, \
                count(*) FILTER (WHERE state = 'draft')::int8, \
                count(*) FILTER (WHERE state = 'waiting')::int8, \
                count(*) FILTER (WHERE state = 'confirmed')::int8, \
                count(*) FILTER (WHERE state = 'partially_available')::int8, \
                count(*) FILTER (WHERE state = 'assigned')::int8, \
                count(*) FILTER (WHERE state = 'done')::int8, \
                count(*) FILTER (WHERE state = 'cancel')::int8, \
                count(*) FILTER (WHERE state = 'done' AND is_scrap)::int8, \
                count(*) FILTER (WHERE state = 'cancel' AND is_scrap)::int8, \
                count(*) FILTER (WHERE state = 'assigned' AND product_uom_qty = 0)::int8, \
                count(*) FILTER (WHERE state NOT IN ('done', 'cancel') AND product_uom_qty <> 0)::int8, \
                count(*) FILTER (WHERE state = 'confirmed' AND product_uom_qty > 0)::int8, \
                count(*) FILTER (WHERE NOT coalesce(is_scrap, false) AND procure_method = 'make_to_order')::int8 \
             FROM stock_move WHERE picking_id = $1",
            &[&picking],
        )
        .await
        .unwrap()
        .remove(0);
    (0..COUNTERS.len()).map(|i| row.get::<_, i64>(i)).collect()
}

async fn assert_folded(tx: &mut Transaction, pickings: &[i64], step: &str) {
    for picking in pickings {
        let (stored, _) = stored_vector(tx, *picking).await;
        assert_eq!(
            stored,
            folded_vector(tx, *picking).await,
            "{step}: transfer {picking}"
        );
    }
}

#[tokio::test]
async fn move_counters_follow_ordinary_move_mutations() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "CNT/A"),
    )
    .await
    .unwrap();
    let b = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "CNT/B"),
    )
    .await
    .unwrap();
    // one create statement adds several moves to two transfers
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, a, "confirmed", "0", false, false),
            counted_move(&f, a, "assigned", "4", false, true),
            counted_move(&f, a, "done", "1", true, false),
            counted_move(&f, b, "cancel", "2", false, false),
            // keeps B open: a cancelled transfer takes no further moves
            counted_move(&f, b, "draft", "1", false, false),
        ],
    )
    .await
    .unwrap();
    assert_folded(&mut tx, &[a, b], "create").await;
    // a demand-only change (0 to 5, no state change) moves the confirmed move into both
    // demand counts
    write_moves(
        &mut tx,
        f.company,
        &moves[..1],
        with_demand(RawVals::new(), "5"),
    )
    .await
    .unwrap();
    assert_folded(&mut tx, &[a, b], "demand").await;
    let (vector, _) = stored_vector(&mut tx, a).await;
    assert_eq!(
        vector[11..13],
        [2_i64, 1],
        "open and confirmed demand counts after 0 -> 5"
    );
    // a state write and a reparent in one write
    write_moves(
        &mut tx,
        f.company,
        &moves[1..2],
        RawVals::new()
            .set_cell("state", text("partially_available"))
            .set_cell("picking_id", Cell::Id(b)),
    )
    .await
    .unwrap();
    assert_folded(&mut tx, &[a, b], "reparent").await;
    // a write naming no counted column changes nothing
    let before = (
        stored_vector(&mut tx, a).await,
        stored_vector(&mut tx, b).await,
    );
    write_moves(
        &mut tx,
        f.company,
        &moves,
        RawVals::new().set_cell("origin", text("SO1")),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            stored_vector(&mut tx, a).await,
            stored_vector(&mut tx, b).await
        ),
        before
    );
    // a trusted raw insert is counted by the same statement
    tx.execute(
        "INSERT INTO stock_move (company_id, product_id, uom_id, location_id, location_dest_id, \
            date, state, procure_method, open_line_version, product_uom_qty, quantity, product_qty, \
            quantity_product_uom, uom_factor, product_uom_factor, product_unit_digits, picking_id, \
            is_scrap, picked) \
         VALUES ($1, $2, $3, $4, $5, now(), 'waiting', 'make_to_stock', 0, 3, 0, 3, 0, 1, 1, 2, $6, \
            false, false)",
        &[&f.company, &f.product, &f.unit, &f.location, &f.other_location, &a],
    )
    .await
    .unwrap();
    assert_folded(&mut tx, &[a, b], "raw insert").await;
    // a delete
    unlink_moves(&mut tx, f.company, &moves[3..]).await.unwrap();
    assert_folded(&mut tx, &[a, b], "unlink").await;
    tx.commit().await.unwrap();
    // a rolled-back mutation leaves the committed vector
    let mut tx = f.h.begin().await.unwrap();
    let committed = (
        stored_vector(&mut tx, a).await,
        stored_vector(&mut tx, b).await,
    );
    create_moves(
        &mut tx,
        f.company,
        vec![counted_move(&f, a, "draft", "1", false, false)],
    )
    .await
    .unwrap();
    assert_folded(&mut tx, &[a], "inside the rolled-back unit").await;
    tx.rollback().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    assert_eq!(
        (
            stored_vector(&mut tx, a).await,
            stored_vector(&mut tx, b).await
        ),
        committed
    );
    assert_folded(&mut tx, &[a, b], "after rollback").await;
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// One state-rule case: shipping policy, bypassing source, moves `(state, demand, scrap, mto)`,
/// the expected state.
type StateCase = (
    &'static str,
    bool,
    &'static [(&'static str, &'static str, bool, bool)],
    &'static str,
);

#[tokio::test]
async fn picking_state_follows_the_reference_rule_from_counters() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let supplier = create(
        &mut tx,
        "stock.location",
        RawVals::new()
            .set_cell("name", text("Vendors"))
            .set_cell("usage", text("supplier")),
    )
    .await;
    let cases: &[StateCase] = &[
        ("direct", false, &[], "draft"),
        (
            "direct",
            false,
            &[
                ("draft", "1", false, false),
                ("assigned", "1", false, false),
            ],
            "draft",
        ),
        (
            "direct",
            false,
            &[("cancel", "1", false, false), ("cancel", "1", false, false)],
            "cancel",
        ),
        (
            "direct",
            false,
            &[("done", "1", false, false), ("cancel", "1", false, false)],
            "done",
        ),
        (
            "direct",
            false,
            &[("done", "1", true, false), ("cancel", "1", false, false)],
            "cancel",
        ),
        (
            "direct",
            false,
            &[
                ("assigned", "5", false, false),
                ("confirmed", "5", false, false),
            ],
            "assigned",
        ),
        (
            "one",
            false,
            &[
                ("assigned", "5", false, false),
                ("confirmed", "5", false, false),
            ],
            "confirmed",
        ),
        (
            "one",
            false,
            &[
                ("confirmed", "0", false, false),
                ("waiting", "0", false, false),
            ],
            "assigned",
        ),
        (
            "direct",
            true,
            &[("confirmed", "5", false, false)],
            "assigned",
        ),
        // the other branches of the ranked rule
        (
            "direct",
            true,
            &[("confirmed", "5", false, true)],
            "confirmed",
        ),
        (
            "direct",
            false,
            &[("assigned", "0", false, false)],
            "assigned",
        ),
        (
            "direct",
            false,
            &[("confirmed", "5", false, false)],
            "confirmed",
        ),
        (
            "direct",
            false,
            &[("confirmed", "0", false, false)],
            "assigned",
        ),
        (
            "direct",
            false,
            &[("confirmed", "-1", false, false)],
            "confirmed",
        ),
        (
            "direct",
            false,
            &[
                ("confirmed", "-1", false, false),
                ("confirmed", "0", false, false),
            ],
            "assigned",
        ),
        (
            "direct",
            false,
            &[
                ("waiting", "5", false, false),
                ("confirmed", "5", false, false),
            ],
            "waiting",
        ),
        (
            "one",
            false,
            &[
                ("waiting", "5", false, false),
                ("assigned", "5", false, false),
            ],
            "waiting",
        ),
        (
            "one",
            false,
            &[
                ("partially_available", "5", false, false),
                ("assigned", "5", false, false),
            ],
            "confirmed",
        ),
    ];
    for (i, (policy, bypass, moves, expected)) in cases.iter().enumerate() {
        let source = if *bypass { supplier } else { f.location };
        let picking = create_picking(
            &mut tx,
            f.company,
            picking_vals(&f, f.company, f.picking_type, &format!("RULE/{i}"))
                .set_cell("move_type", text(policy))
                .set_cell("location_id", Cell::Id(source)),
        )
        .await
        .unwrap();
        let vals = moves
            .iter()
            .map(|(state, qty, scrap, mto)| counted_move(&f, picking, state, qty, *scrap, *mto))
            .collect::<Vec<_>>();
        if !vals.is_empty() {
            create_moves(&mut tx, f.company, vals).await.unwrap();
        }
        assert_folded(&mut tx, &[picking], &format!("case {i}")).await;
        let (_, state) = stored_vector(&mut tx, picking).await;
        assert_eq!(state, *expected, "case {i}: {policy} {moves:?}");
        let cached = tx
            .env()
            .records("stock.picking")
            .unwrap()
            .browse([picking])
            .unwrap()
            .cell(&mut tx, "state")
            .await
            .unwrap();
        assert_eq!(
            cached,
            text(expected),
            "case {i}: the ORM reads the stored state"
        );
    }
    // a policy change re-evaluates the state in its own statement (case 6 becomes direct)
    let picking: i64 = tx
        .query("SELECT id FROM stock_picking WHERE name = 'RULE/6'", &[])
        .await
        .unwrap()[0]
        .get(0);
    let records = tx
        .env()
        .records("stock.picking")
        .unwrap()
        .browse([picking])
        .unwrap();
    records
        .write(
            &mut tx,
            RawVals::new().set_cell("move_type", text("direct")),
        )
        .await
        .unwrap();
    assert_eq!(
        records.cell(&mut tx, "state").await.unwrap(),
        text("assigned")
    );
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn picking_derived_columns_are_refused_by_name() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "DER/1"),
    )
    .await
    .unwrap();
    let label = |tx: &Transaction, field: &str| {
        tx.registry()
            .model("stock.picking")
            .unwrap()
            .field(field)
            .unwrap()
            .string
            .to_string()
    };
    for (field, value) in [
        ("state", text("done")),
        ("move_count", Cell::Int(0)),
        ("mto_nonscrap_count", Cell::Int(0)),
    ] {
        let expected = erp_addon_stock::StockError::DerivedFromMoves {
            field: label(&tx, field),
            default: false,
        }
        .to_string();
        let error = create_picking(
            &mut tx,
            f.company,
            picking_vals(&f, f.company, f.picking_type, &format!("DER/{field}"))
                .set_cell(field, value.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::Programming,
            "create {field}: {error:?}"
        );
        assert_eq!(error.message(), expected);
        for sudo in [false, true] {
            let error = tx
                .savepoint(async |tx| {
                    tx.env()
                        .sudo(sudo)
                        .records("stock.picking")?
                        .browse([picking])?
                        .write(tx, RawVals::new().set_cell(field, value.clone()))
                        .await
                })
                .await
                .unwrap_err();
            assert_eq!(
                error.kind(),
                ErrorKind::Programming,
                "write {field} sudo={sudo}"
            );
            assert_eq!(error.message(), expected);
        }
    }
    let error = tx
        .savepoint(async |tx| {
            tx.env()
                .with_company(f.company)?
                .with_context([("default_state", Value::str("done"))])
                .records("stock.picking")?
                .create(
                    tx,
                    vec![picking_vals(&f, f.company, f.picking_type, "DER/default")],
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming, "{error:?}");
    assert_eq!(
        error.message(),
        erp_addon_stock::StockError::DerivedFromMoves {
            field: label(&tx, "state"),
            default: true,
        }
        .to_string()
    );
    let (vector, state) = stored_vector(&mut tx, picking).await;
    assert_eq!((vector, state.as_str()), (vec![0; COUNTERS.len()], "draft"));
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn move_counters_reach_cached_reads_and_the_overview() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let id = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "CACHE/1"),
    )
    .await
    .unwrap();
    let picking = tx
        .env()
        .records("stock.picking")
        .unwrap()
        .browse([id])
        .unwrap();
    let operation = tx
        .env()
        .records("stock.picking.type")
        .unwrap()
        .browse([f.picking_type])
        .unwrap();
    // preload every value the move changes
    assert_eq!(picking.cell(&mut tx, "state").await.unwrap(), text("draft"));
    assert_eq!(
        picking.cell(&mut tx, "assigned_count").await.unwrap(),
        Cell::Int(0)
    );
    assert_eq!(
        operation
            .cell(&mut tx, "count_picking_ready")
            .await
            .unwrap(),
        Cell::Int(0)
    );
    assert_eq!(
        operation
            .cell(&mut tx, "count_picking_draft")
            .await
            .unwrap(),
        Cell::Int(1)
    );
    create_moves(
        &mut tx,
        f.company,
        vec![counted_move(&f, id, "assigned", "2", false, false)],
    )
    .await
    .unwrap();
    assert_eq!(
        picking.cell(&mut tx, "state").await.unwrap(),
        text("assigned")
    );
    assert_eq!(
        picking.cell(&mut tx, "assigned_count").await.unwrap(),
        Cell::Int(1)
    );
    assert_eq!(
        picking.cell(&mut tx, "move_count").await.unwrap(),
        Cell::Int(1)
    );
    assert_eq!(
        operation
            .cell(&mut tx, "count_picking_ready")
            .await
            .unwrap(),
        Cell::Int(1)
    );
    assert_eq!(
        operation
            .cell(&mut tx, "count_picking_draft")
            .await
            .unwrap(),
        Cell::Int(0)
    );
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let operation = tx
        .env()
        .records("stock.picking.type")
        .unwrap()
        .browse([f.picking_type])
        .unwrap();
    assert_eq!(
        operation
            .cell(&mut tx, "count_picking_ready")
            .await
            .unwrap(),
        Cell::Int(1)
    );
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The counters and the state of each of `pickings` and the overview counts of `picking_type`, as
/// the ORM reads them in `tx`, against the stored columns: the transfer's own vector and state, and
/// the number of stored transfers of the operation type per overview state.
async fn assert_cached_follows_stored(
    tx: &mut Transaction,
    pickings: &[i64],
    picking_type: i64,
    step: &str,
) {
    for id in pickings {
        let (vector, state) = stored_vector(tx, *id).await;
        let picking = tx
            .env()
            .records("stock.picking")
            .unwrap()
            .browse([*id])
            .unwrap();
        for (name, stored) in COUNTERS.into_iter().zip(vector) {
            assert_eq!(
                picking.cell(tx, name).await.unwrap(),
                Cell::Int(i32::try_from(stored).unwrap()),
                "{step}: cached {name} of transfer {id}"
            );
        }
        assert_eq!(
            picking.cell(tx, "state").await.unwrap(),
            text(&state),
            "{step}: cached state of transfer {id}"
        );
    }
    let row = tx
        .query(
            "SELECT count(*) FILTER (WHERE state::text = 'draft')::int4, \
                count(*) FILTER (WHERE state::text IN ('confirmed', 'waiting'))::int4, \
                count(*) FILTER (WHERE state::text = 'assigned')::int4 \
             FROM stock_picking WHERE picking_type_id = $1",
            &[&picking_type],
        )
        .await
        .unwrap()
        .remove(0);
    let operation = tx
        .env()
        .records("stock.picking.type")
        .unwrap()
        .browse([picking_type])
        .unwrap();
    let counts = [
        "count_picking_draft",
        "count_picking_waiting",
        "count_picking_ready",
    ];
    for (i, name) in counts.into_iter().enumerate() {
        assert_eq!(
            operation.cell(tx, name).await.unwrap(),
            Cell::Int(row.get::<_, i32>(i)),
            "{step}: cached {name} of the operation type"
        );
    }
}

#[tokio::test]
async fn move_writes_reparents_and_unlinks_reach_cached_reads() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "CACHE/A"),
    )
    .await
    .unwrap();
    let b = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "CACHE/B"),
    )
    .await
    .unwrap();
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, a, "confirmed", "0", false, false),
            counted_move(&f, b, "draft", "1", false, false),
        ],
    )
    .await
    .unwrap();
    let (counted, spare) = (moves[0], moves[1]);
    // each check reads, and so caches, every value the next step changes
    assert_cached_follows_stored(&mut tx, &[a, b], f.picking_type, "before").await;
    assert_eq!(stored_vector(&mut tx, a).await.1, "assigned");
    assert_eq!(stored_vector(&mut tx, b).await.1, "draft");
    // a demand-only write: the confirmed move's zero demand becomes positive, the transfer waits
    write_moves(
        &mut tx,
        f.company,
        &[counted],
        with_demand(RawVals::new(), "5"),
    )
    .await
    .unwrap();
    assert_eq!(stored_vector(&mut tx, a).await.1, "confirmed");
    assert_cached_follows_stored(&mut tx, &[a, b], f.picking_type, "demand write").await;
    // a reparent: the old transfer loses its only move, the new one counts two
    write_moves(
        &mut tx,
        f.company,
        &[counted],
        RawVals::new().set_cell("picking_id", Cell::Id(b)),
    )
    .await
    .unwrap();
    let (old, new) = (
        stored_vector(&mut tx, a).await,
        stored_vector(&mut tx, b).await,
    );
    assert_eq!((old.0[0], old.1.as_str()), (0, "draft"));
    assert_eq!((new.0[0], new.1.as_str()), (2, "draft"));
    assert_cached_follows_stored(&mut tx, &[a, b], f.picking_type, "reparent").await;
    // an unlink: without its draft move the new transfer follows the confirmed one
    unlink_moves(&mut tx, f.company, &[spare]).await.unwrap();
    let left = stored_vector(&mut tx, b).await;
    assert_eq!((left.0[0], left.1.as_str()), (1, "confirmed"));
    assert_cached_follows_stored(&mut tx, &[a, b], f.picking_type, "unlink").await;
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn concurrent_move_writers_serialize_on_transfer_roots() {
    let f = fixture().await;
    let mut setup = f.h.begin().await.unwrap();
    let p1 = create_picking(
        &mut setup,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "ROOT/1"),
    )
    .await
    .unwrap();
    let p2 = create_picking(
        &mut setup,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "ROOT/2"),
    )
    .await
    .unwrap();
    let moves = create_moves(
        &mut setup,
        f.company,
        vec![
            counted_move(&f, p1, "confirmed", "1", false, false),
            counted_move(&f, p2, "confirmed", "1", false, false),
        ],
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();
    drop(setup);
    let retryable = |error: &Error| {
        assert_eq!(error.kind(), ErrorKind::Concurrency, "{error:?}");
        assert_eq!(error.sqlstate(), Some("40001"), "{error:?}");
    };
    // same transfer: B's snapshot predates A's commit, so B's root lock refuses retryably
    let mut a = f.h.begin().await.unwrap();
    let mut b = f.h.begin().await.unwrap();
    b.query("SELECT 1", &[]).await.unwrap();
    create_moves(
        &mut a,
        f.company,
        vec![counted_move(&f, p1, "draft", "1", false, false)],
    )
    .await
    .unwrap();
    a.commit().await.unwrap();
    drop(a);
    let error = create_moves(
        &mut b,
        f.company,
        vec![counted_move(&f, p1, "draft", "1", false, false)],
    )
    .await
    .unwrap_err();
    retryable(&error);
    b.rollback().await.unwrap();
    drop(b);
    // opposite reparents: A moves the first move to P2, B the second to P1; both lock P1 then P2
    let mut a = f.h.begin().await.unwrap();
    let mut b = f.h.begin().await.unwrap();
    b.query("SELECT 1", &[]).await.unwrap();
    write_moves(
        &mut a,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("picking_id", Cell::Id(p2)),
    )
    .await
    .unwrap();
    a.commit().await.unwrap();
    drop(a);
    let error = write_moves(
        &mut b,
        f.company,
        &moves[1..],
        RawVals::new().set_cell("picking_id", Cell::Id(p1)),
    )
    .await
    .unwrap_err();
    retryable(&error);
    b.rollback().await.unwrap();
    drop(b);
    // the retry on a fresh snapshot succeeds, and every vector equals its fold
    let mut retry = f.h.begin().await.unwrap();
    write_moves(
        &mut retry,
        f.company,
        &moves[1..],
        RawVals::new().set_cell("picking_id", Cell::Id(p1)),
    )
    .await
    .unwrap();
    retry.commit().await.unwrap();
    drop(retry);
    let mut check = f.h.begin().await.unwrap();
    assert_folded(&mut check, &[p1, p2], "after the serialized writers").await;
    assert_eq!(stored_vector(&mut check, p1).await.0[0], 2);
    assert_eq!(stored_vector(&mut check, p2).await.0[0], 1);
    check.rollback().await.unwrap();
    drop(check);
    f.h.cleanup().await.unwrap();
}

/// The error of a second transaction asking, without waiting, for the root-lock mode on row `id`
/// of `table`; `None` when the row is free. A foreign-key check's key-share lock does not refuse
/// this mode, so only a row lock or a row update of another transaction does.
async fn row_lock_error(h: &Harness, table: &str, id: i64) -> Option<Error> {
    let mut tx = h.begin().await.unwrap();
    let sql = format!("SELECT FROM {table} WHERE id = $1 FOR NO KEY UPDATE NOWAIT");
    let error = tx.query(&sql, &[&id]).await.err();
    tx.rollback().await.unwrap();
    drop(tx);
    error
}

fn assert_locked(error: Option<Error>, what: &str) {
    let error = error.unwrap_or_else(|| panic!("{what} was not locked"));
    assert_eq!(error.sqlstate(), Some("55P03"), "{what}: {error:?}");
}

/// the root locks themselves, seen from a second transaction before anything is
/// written. The typed lock holds the old and the new transfer and the move, and nothing else; a
/// move write naming a counted column holds its transfer even when the counter trigger has no
/// changed row and so touches no transfer.
#[tokio::test]
async fn move_writers_hold_their_root_locks() {
    let f = fixture().await;
    let mut setup = f.h.begin().await.unwrap();
    let mut pickings = Vec::new();
    for name in ["LOCK/1", "LOCK/2", "LOCK/3"] {
        pickings.push(
            create_picking(
                &mut setup,
                f.company,
                picking_vals(&f, f.company, f.picking_type, name),
            )
            .await
            .unwrap(),
        );
    }
    let (p1, p2, p3) = (pickings[0], pickings[1], pickings[2]);
    let moves = create_moves(
        &mut setup,
        f.company,
        vec![
            counted_move(&f, p1, "confirmed", "1", false, false),
            counted_move(&f, p2, "confirmed", "1", false, false),
        ],
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();
    drop(setup);
    // the typed lock of a reparent of the first move to the third transfer, with no write at all
    let mut a = f.h.begin().await.unwrap();
    let roots = erp_addon_stock::move_counters::lock_move_roots(&mut a, &moves[..1], &[p3])
        .await
        .unwrap();
    let mut want = vec![p1, p3];
    want.sort_unstable();
    assert_eq!(roots, want);
    assert_locked(
        row_lock_error(&f.h, "stock_picking", p1).await,
        "the move's transfer",
    );
    assert_locked(
        row_lock_error(&f.h, "stock_picking", p3).await,
        "the new transfer",
    );
    assert_locked(
        row_lock_error(&f.h, "stock_move", moves[0]).await,
        "the move",
    );
    let free = row_lock_error(&f.h, "stock_picking", p2).await;
    assert!(free.is_none(), "a transfer outside the roots: {free:?}");
    let free = row_lock_error(&f.h, "stock_move", moves[1]).await;
    assert!(free.is_none(), "a move outside the call: {free:?}");
    a.rollback().await.unwrap();
    drop(a);
    let free = row_lock_error(&f.h, "stock_picking", p1).await;
    assert!(free.is_none(), "after the holder ended: {free:?}");
    // the transfer lock a move create takes before its insert
    let mut a = f.h.begin().await.unwrap();
    erp_addon_stock::move_counters::lock_pickings(&mut a, &[p2])
        .await
        .unwrap();
    assert_locked(
        row_lock_error(&f.h, "stock_picking", p2).await,
        "the named transfer",
    );
    let free = row_lock_error(&f.h, "stock_picking", p1).await;
    assert!(free.is_none(), "a transfer not named: {free:?}");
    a.rollback().await.unwrap();
    drop(a);
    // an ORM write of a counted column with its stored value: no counter changes, so the trigger
    // neither locks nor updates the transfer, and only the write's own root lock can hold it
    let mut a = f.h.begin().await.unwrap();
    let before = stored_vector(&mut a, p1).await;
    write_moves(
        &mut a,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("procure_method", text("make_to_stock")),
    )
    .await
    .unwrap();
    assert_eq!(stored_vector(&mut a, p1).await, before);
    assert_locked(
        row_lock_error(&f.h, "stock_picking", p1).await,
        "the written move's transfer",
    );
    assert_locked(
        row_lock_error(&f.h, "stock_move", moves[0]).await,
        "the written move",
    );
    let free = row_lock_error(&f.h, "stock_picking", p2).await;
    assert!(
        free.is_none(),
        "a transfer the write does not touch: {free:?}"
    );
    a.rollback().await.unwrap();
    drop(a);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn source_bypass_change_is_refused_while_transfers_are_open() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "BYP/1"),
    )
    .await
    .unwrap();
    create_moves(
        &mut tx,
        f.company,
        vec![counted_move(&f, picking, "confirmed", "5", false, false)],
    )
    .await
    .unwrap();
    let message = "The location type cannot switch between a type that reserves stock and one \
                   that does not while transfers leaving from it are open.";
    let error = tx
        .savepoint(async |tx| {
            tx.execute(
                "UPDATE stock_location SET usage = 'supplier' WHERE id = $1",
                &[&f.location],
            )
            .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert_eq!(error.sqlstate(), Some("ERP04"));
    assert_eq!(
        error.constraint(),
        Some("stock_location_bypass_open_transfers")
    );
    assert_eq!(error.message(), message);
    // a usage change that keeps the bypass class is allowed
    tx.savepoint(async |tx| {
        tx.execute(
            "UPDATE stock_location SET usage = 'transit' WHERE id = $1",
            &[&f.location],
        )
        .await
    })
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The shipped stock 1.6 backfill (lead condition M3).
const BACKFILL_SQL: &str = include_str!("../migrations/1.6/post-move-counters.sql");

#[tokio::test]
async fn backfill_sets_vectors_with_the_triggers_installed() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let ready = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "FILL/1"),
    )
    .await
    .unwrap();
    let done = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "FILL/2"),
    )
    .await
    .unwrap();
    let empty = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "FILL/3"),
    )
    .await
    .unwrap();
    create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, ready, "assigned", "5", false, false),
            counted_move(&f, ready, "confirmed", "5", false, true),
            counted_move(&f, done, "done", "1", false, false),
            counted_move(&f, done, "cancel", "1", true, false),
        ],
    )
    .await
    .unwrap();
    let expected: Vec<(Vec<i64>, String)> = vec![
        stored_vector(&mut tx, ready).await,
        stored_vector(&mut tx, done).await,
        stored_vector(&mut tx, empty).await,
    ];
    assert_eq!(
        expected.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(),
        ["assigned", "done", "draft"]
    );
    // a pre-1.6 shape: vectors that do not match the moves (a trusted raw write; the state
    // trigger then evaluates the corrupted vector)
    tx.execute(
        "UPDATE stock_picking SET move_count = 0, draft_count = 0, waiting_count = 0, \
            confirmed_count = 0, partially_available_count = 0, assigned_count = 0, done_count = 0, \
            cancel_count = 0, done_scrap_count = 0, cancel_scrap_count = 0, \
            assigned_zero_demand_count = 0, open_positive_demand_count = 0, \
            confirmed_positive_demand_count = 0, mto_nonscrap_count = 0 WHERE id = ANY($1)",
        &[&vec![ready, done, empty]],
    )
    .await
    .unwrap();
    assert_eq!(stored_vector(&mut tx, ready).await.1, "draft");
    tx.batch(BACKFILL_SQL).await.unwrap();
    for (picking, want) in [ready, done, empty].into_iter().zip(&expected) {
        assert_eq!(
            &stored_vector(&mut tx, picking).await,
            want,
            "transfer {picking}"
        );
    }
    assert_folded(&mut tx, &[ready, done, empty], "backfill").await;
    let versions = "SELECT ctid::text FROM stock_picking WHERE id = ANY($1) ORDER BY id";
    let ids = vec![ready, done, empty];
    let before: Vec<String> = tx
        .query(versions, &[&ids])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    tx.batch(BACKFILL_SQL).await.unwrap();
    let after: Vec<String> = tx
        .query(versions, &[&ids])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(after, before, "a second backfill writes no transfer row");
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The shipped stock 1.7 projection rebuild, which an operator repeats after an upgrade that ran
/// with older nodes still writing.
const REFERENCES_SQL: &str = include_str!("../migrations/1.7/post-move-references.sql");

#[tokio::test]
async fn reference_rebuild_repairs_the_projection_and_repeats_safely() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "FIX/A"),
    )
    .await
    .unwrap();
    let refs = create_references(&mut tx, &["SO/F1", "SO/F2"]).await;
    create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, a, "draft", "1", false, false)
                .set_cell("reference_ids", id_list(&refs[..1])),
        ],
    )
    .await
    .unwrap();
    // drift: the supported pair is missing and an unsupported one is present (trusted raw writes)
    tx.execute(
        "DELETE FROM stock_reference_picking_rel WHERE picking_id = $1",
        &[&a],
    )
    .await
    .unwrap();
    tx.execute(
        "INSERT INTO stock_reference_picking_rel (reference_id, picking_id) VALUES ($1, $2)",
        &[&refs[1], &a],
    )
    .await
    .unwrap();
    tx.batch(REFERENCES_SQL).await.unwrap();
    tx.invalidate_model("stock.picking", None).await.unwrap();
    assert_projection(&mut tx, a, &refs[..1], "rebuilt").await;
    tx.batch(REFERENCES_SQL).await.unwrap();
    assert_projection(&mut tx, a, &refs[..1], "repeated").await;
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

// ---- move references and chains (topology ADR D3) ----

fn id_list(ids: &[i64]) -> Cell {
    Cell::Ids(
        ids.iter()
            .map(|id| RecId::from_real(*id).unwrap())
            .collect(),
    )
}

async fn create_references(tx: &mut Transaction, names: &[&str]) -> Vec<i64> {
    let vals = names
        .iter()
        .map(|name| RawVals::new().set_cell("name", text(name)))
        .collect();
    tx.env()
        .records("stock.reference")
        .unwrap()
        .create(tx, vals)
        .await
        .unwrap()
        .ids()
}

async fn write_reference(tx: &mut Transaction, reference: i64, vals: RawVals) -> Result<()> {
    tx.savepoint(async |tx| {
        tx.env()
            .records("stock.reference")?
            .browse([reference])?
            .write(tx, vals)
            .await
    })
    .await
}

/// The stored projection of `picking`, ascending.
async fn projection(tx: &mut Transaction, picking: i64) -> Vec<i64> {
    let sql =
        "SELECT reference_id FROM stock_reference_picking_rel WHERE picking_id = $1 ORDER BY 1";
    tx.query(sql, &[&picking])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// An independent fold of the references of the moves of `picking`, ascending.
async fn reference_union(tx: &mut Transaction, picking: i64) -> Vec<i64> {
    let sql = "SELECT DISTINCT r.reference_id FROM stock_reference_move_rel r \
               JOIN stock_move m ON m.id = r.move_id WHERE m.picking_id = $1 ORDER BY 1";
    tx.query(sql, &[&picking])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// The ids of the x2many `field` of record `id` as the ORM reads it in `tx`, ascending.
async fn orm_ids(tx: &mut Transaction, model: &str, id: i64, field: &str) -> Vec<i64> {
    let cell = tx
        .env()
        .records(model)
        .unwrap()
        .browse([id])
        .unwrap()
        .cell(tx, field)
        .await
        .unwrap();
    let Cell::Ids(ids) = cell else {
        panic!("{model}.{field} is not an id list: {cell:?}");
    };
    let mut out: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    out.sort_unstable();
    out
}

async fn assert_projection(tx: &mut Transaction, picking: i64, want: &[i64], step: &str) {
    let stored = projection(tx, picking).await;
    assert_eq!(stored, reference_union(tx, picking).await, "{step}: fold");
    assert_eq!(stored, want, "{step}: stored");
    assert_eq!(
        orm_ids(tx, "stock.picking", picking, "reference_ids").await,
        want,
        "{step}: cached read"
    );
}

#[tokio::test]
async fn picking_references_are_the_union_of_their_moves() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "REF/A"),
    )
    .await
    .unwrap();
    let b = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "REF/B"),
    )
    .await
    .unwrap();
    let refs = create_references(&mut tx, &["SO/1", "SO/2"]).await;
    let (r1, r2) = (refs[0], refs[1]);
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, a, "draft", "1", false, false),
            counted_move(&f, a, "draft", "1", false, false),
            counted_move(&f, b, "draft", "1", false, false),
        ],
    )
    .await
    .unwrap();
    let (m1, m2, m3) = (moves[0], moves[1], moves[2]);
    // the move side of the relation
    write_moves(
        &mut tx,
        f.company,
        &[m1],
        RawVals::new().set_cell("reference_ids", id_list(&[r1])),
    )
    .await
    .unwrap();
    assert_projection(&mut tx, a, &[r1], "move side").await;
    assert_projection(&mut tx, b, &[], "move side").await;
    // the reference side of the relation
    write_reference(
        &mut tx,
        r2,
        RawVals::new().set_cell("move_ids", id_list(&[m2, m3])),
    )
    .await
    .unwrap();
    assert_projection(&mut tx, a, &[r1, r2], "reference side").await;
    assert_projection(&mut tx, b, &[r2], "reference side").await;
    assert_eq!(
        orm_ids(&mut tx, "stock.reference", r2, "picking_ids").await,
        vec![a, b]
    );
    // a move changing transfer takes its references along and keeps them
    write_moves(
        &mut tx,
        f.company,
        &[m2],
        RawVals::new().set_cell("picking_id", Cell::Id(b)),
    )
    .await
    .unwrap();
    assert_projection(&mut tx, a, &[r1], "reparent").await;
    assert_projection(&mut tx, b, &[r2], "reparent").await;
    assert_eq!(
        orm_ids(&mut tx, "stock.move", m2, "reference_ids").await,
        vec![r2]
    );
    // deleting one of several supporting moves keeps the pair, the last one removes it
    unlink_moves(&mut tx, f.company, &[m3]).await.unwrap();
    assert_projection(&mut tx, b, &[r2], "one of two removed").await;
    write_moves(
        &mut tx,
        f.company,
        &[m2],
        RawVals::new().set_cell("reference_ids", id_list(&[])),
    )
    .await
    .unwrap();
    assert_projection(&mut tx, b, &[], "last support removed").await;
    assert_projection(&mut tx, a, &[r1], "untouched").await;
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn reference_projection_is_refused_by_name() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "PRJ/1"),
    )
    .await
    .unwrap();
    let reference = create_references(&mut tx, &["SO/PRJ"]).await[0];
    let label = |tx: &Transaction, model: &str, field: &str| {
        tx.registry()
            .model(model)
            .unwrap()
            .field(field)
            .unwrap()
            .string
            .to_string()
    };
    let refused = |field: String, default: bool| {
        erp_addon_stock::StockError::ReferenceProjection { field, default }.to_string()
    };
    let picking_refs = label(&tx, "stock.picking", "reference_ids");
    let reference_pickings = label(&tx, "stock.reference", "picking_ids");
    for sudo in [false, true] {
        let error = tx
            .savepoint(async |tx| {
                tx.env()
                    .sudo(sudo)
                    .records("stock.picking")?
                    .browse([picking])?
                    .write(
                        tx,
                        RawVals::new().set_cell("reference_ids", id_list(&[reference])),
                    )
                    .await
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::Programming,
            "sudo={sudo}: {error:?}"
        );
        assert_eq!(error.message(), refused(picking_refs.clone(), false));
        let error = tx
            .savepoint(async |tx| {
                tx.env()
                    .sudo(sudo)
                    .records("stock.reference")?
                    .browse([reference])?
                    .write(
                        tx,
                        RawVals::new().set_cell("picking_ids", id_list(&[picking])),
                    )
                    .await
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::Programming,
            "sudo={sudo}: {error:?}"
        );
        assert_eq!(error.message(), refused(reference_pickings.clone(), false));
    }
    let error = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "PRJ/2")
            .set_cell("reference_ids", id_list(&[reference])),
    )
    .await
    .unwrap_err();
    assert_eq!(error.message(), refused(picking_refs.clone(), false));
    let error = tx
        .savepoint(async |tx| {
            tx.env()
                .records("stock.reference")?
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", text("SO/PRJ2"))
                            .set_cell("picking_ids", id_list(&[picking])),
                    ],
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.message(), refused(reference_pickings, false));
    let error = tx
        .savepoint(async |tx| {
            tx.env()
                .with_company(f.company)?
                .with_context([(
                    "default_reference_ids",
                    Value::List(vec![Value::Int(reference)].into()),
                )])
                .records("stock.picking")?
                .create(
                    tx,
                    vec![picking_vals(&f, f.company, f.picking_type, "PRJ/3")],
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming, "{error:?}");
    assert_eq!(error.message(), refused(picking_refs, true));
    assert_eq!(projection(&mut tx, picking).await, Vec::<i64>::new());
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The form RPC, data files and `copy` deliver x2many values as commands, never as a plain id list.
#[tokio::test]
async fn reference_commands_are_refused_or_rebuilt_like_plain_values() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "CMD/A"),
    )
    .await
    .unwrap();
    let refs = create_references(&mut tx, &["SO/C1", "SO/C2"]).await;
    let (r1, r2) = (refs[0], refs[1]);
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, a, "draft", "1", false, false),
            counted_move(&f, a, "draft", "1", false, false),
        ],
    )
    .await
    .unwrap();
    write_moves(
        &mut tx,
        f.company,
        &moves[..1],
        RawVals::new().set_commands("reference_ids", [Command::set(rec_ids(&[r1]))]),
    )
    .await
    .unwrap();
    assert_projection(&mut tx, a, &[r1], "move side, commands").await;
    write_reference(
        &mut tx,
        r2,
        RawVals::new().set_commands("move_ids", [Command::link(rec_ids(&moves[1..])[0])]),
    )
    .await
    .unwrap();
    assert_projection(&mut tx, a, &[r1, r2], "reference write, commands").await;
    let r3 = tx
        .savepoint(async |tx| {
            Ok(tx
                .env()
                .records("stock.reference")?
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", text("SO/C3"))
                            .set_commands("move_ids", [Command::set(rec_ids(&moves))]),
                    ],
                )
                .await?
                .ids()[0])
        })
        .await
        .unwrap();
    let mut all = vec![r1, r2, r3];
    all.sort_unstable();
    assert_projection(&mut tx, a, &all, "reference create, commands").await;
    let refused = |model: &str, field: &str, tx: &Transaction| {
        let field = tx
            .registry()
            .model(model)
            .unwrap()
            .field(field)
            .unwrap()
            .string
            .to_string();
        erp_addon_stock::StockError::ReferenceProjection {
            field,
            default: false,
        }
        .to_string()
    };
    let error = tx
        .savepoint(async |tx| {
            tx.env()
                .records("stock.picking")?
                .browse([a])?
                .write(
                    tx,
                    RawVals::new().set_commands("reference_ids", [Command::Clear]),
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        refused("stock.picking", "reference_ids", &tx)
    );
    let error = tx
        .savepoint(async |tx| {
            tx.env()
                .records("stock.reference")?
                .browse([r1])?
                .write(
                    tx,
                    RawVals::new().set_commands("picking_ids", [Command::Clear]),
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        refused("stock.reference", "picking_ids", &tx)
    );
    let error = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "CMD/B")
            .set_commands("reference_ids", [Command::set(rec_ids(&[r1]))]),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.message(),
        refused("stock.picking", "reference_ids", &tx)
    );
    let error = tx
        .savepoint(async |tx| {
            tx.env()
                .records("stock.reference")?
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("name", text("SO/C4"))
                            .set_commands("picking_ids", [Command::set(rec_ids(&[a]))]),
                    ],
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        refused("stock.reference", "picking_ids", &tx)
    );
    assert_projection(&mut tx, a, &all, "after the refusals").await;
    // a copy of the transfer has no move, so no reference
    let mut defaults = erp_sdk::ValueMap::new();
    defaults.insert("name".into(), Value::Str("CMD/A copy".into()));
    let copy = tx
        .env()
        .with_company(f.company)
        .unwrap()
        .records("stock.picking")
        .unwrap()
        .browse([a])
        .unwrap()
        .copy(&mut tx, Some(defaults))
        .await
        .unwrap()
        .ids()[0];
    assert_projection(&mut tx, copy, &[], "copy").await;
    assert_projection(&mut tx, a, &all, "copy source").await;
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_linked_move_without_references_takes_the_transfers() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "INH/A"),
    )
    .await
    .unwrap();
    let b = create_picking(
        &mut tx,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "INH/B"),
    )
    .await
    .unwrap();
    let refs = create_references(&mut tx, &["SO/I1", "SO/I2"]).await;
    let (r1, r2) = (refs[0], refs[1]);
    // one create: the first move carries R1, the second none and takes it from its transfer
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, a, "draft", "1", false, false)
                .set_cell("reference_ids", id_list(&[r1])),
            counted_move(&f, a, "draft", "1", false, false),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        orm_ids(&mut tx, "stock.move", moves[1], "reference_ids").await,
        vec![r1]
    );
    // an existing nonempty set is retained
    let own = create_moves(
        &mut tx,
        f.company,
        vec![
            counted_move(&f, a, "draft", "1", false, false)
                .set_cell("reference_ids", id_list(&[r2])),
        ],
    )
    .await
    .unwrap()[0];
    assert_eq!(
        orm_ids(&mut tx, "stock.move", own, "reference_ids").await,
        vec![r2]
    );
    // a move without references relinked to A takes A's set
    let bare = create_moves(
        &mut tx,
        f.company,
        vec![counted_move(&f, b, "draft", "1", false, false)],
    )
    .await
    .unwrap()[0];
    assert_eq!(
        orm_ids(&mut tx, "stock.move", bare, "reference_ids").await,
        Vec::<i64>::new()
    );
    write_moves(
        &mut tx,
        f.company,
        &[bare],
        RawVals::new().set_cell("picking_id", Cell::Id(a)),
    )
    .await
    .unwrap();
    assert_eq!(
        orm_ids(&mut tx, "stock.move", bare, "reference_ids").await,
        vec![r1, r2]
    );
    assert_projection(&mut tx, a, &[r1, r2], "after inheritance").await;
    assert_projection(&mut tx, b, &[], "after inheritance").await;
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn concurrent_reference_writers_retry_on_the_transfer() {
    let f = fixture().await;
    let mut setup = f.h.begin().await.unwrap();
    let a = create_picking(
        &mut setup,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "REFC/A"),
    )
    .await
    .unwrap();
    let moves = create_moves(
        &mut setup,
        f.company,
        vec![
            counted_move(&f, a, "draft", "1", false, false),
            counted_move(&f, a, "draft", "1", false, false),
        ],
    )
    .await
    .unwrap();
    let refs = create_references(&mut setup, &["SO/C1", "SO/C2"]).await;
    setup.commit().await.unwrap();
    drop(setup);
    let mut first = f.h.begin().await.unwrap();
    let mut second = f.h.begin().await.unwrap();
    second.query("SELECT 1", &[]).await.unwrap();
    write_moves(
        &mut first,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("reference_ids", id_list(&refs[..1])),
    )
    .await
    .unwrap();
    first.commit().await.unwrap();
    drop(first);
    let error = write_moves(
        &mut second,
        f.company,
        &moves[1..],
        RawVals::new().set_cell("reference_ids", id_list(&refs[1..])),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Concurrency, "{error:?}");
    assert_eq!(error.sqlstate(), Some("40001"), "{error:?}");
    second.rollback().await.unwrap();
    drop(second);
    let mut retry = f.h.begin().await.unwrap();
    write_moves(
        &mut retry,
        f.company,
        &moves[1..],
        RawVals::new().set_cell("reference_ids", id_list(&refs[1..])),
    )
    .await
    .unwrap();
    retry.commit().await.unwrap();
    drop(retry);
    let mut check = f.h.begin().await.unwrap();
    assert_projection(&mut check, a, &refs, "after the retry").await;
    check.rollback().await.unwrap();
    drop(check);
    f.h.cleanup().await.unwrap();
}

fn day(n: i64) -> Cell {
    Cell::DateTime(DateTime::from_micros(
        1_800_000_000_000_000 + n * 86_400_000_000,
    ))
}

async fn deadline_of(tx: &mut Transaction, id: i64) -> (Option<DateTime>, Cell) {
    let rows = tx
        .query("SELECT date_deadline FROM stock_move WHERE id = $1", &[&id])
        .await
        .unwrap();
    let cached = tx
        .env()
        .records("stock.move")
        .unwrap()
        .browse([id])
        .unwrap()
        .cell(tx, "date_deadline")
        .await
        .unwrap();
    (rows[0].get(0), cached)
}

async fn assert_deadline(tx: &mut Transaction, id: i64, want: Cell, step: &str) {
    let (stored, cached) = deadline_of(tx, id).await;
    let stored = stored.map_or(Cell::Null, Cell::DateTime);
    assert_eq!(stored, want, "{step}: stored deadline of {id}");
    assert_eq!(cached, want, "{step}: cached deadline of {id}");
}

#[tokio::test]
async fn a_deadline_shift_reaches_each_open_linked_move_once() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let mv = |state: &str, deadline: Cell| {
        with_demand(move_vals(&f, f.company), "1")
            .set_cell("state", text(state))
            .set_cell("date_deadline", deadline)
    };
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            mv("confirmed", day(0)),
            mv("confirmed", day(2)),
            mv("done", day(1)),
            mv("cancel", day(5)),
            mv("waiting", day(9)),
        ],
    )
    .await
    .unwrap();
    let (m1, m2, done, cancelled, behind) = (moves[0], moves[1], moves[2], moves[3], moves[4]);
    // done -> M1 -> M2 -> M1 (a cycle), M2 -> cancelled -> behind
    for (orig, dests) in [
        (done, vec![m1]),
        (m1, vec![m2]),
        (m2, vec![m1, cancelled]),
        (cancelled, vec![behind]),
    ] {
        write_moves(
            &mut tx,
            f.company,
            &[orig],
            RawVals::new().set_cell("move_dest_ids", id_list(&dests)),
        )
        .await
        .unwrap();
    }
    write_moves(
        &mut tx,
        f.company,
        &[m1],
        RawVals::new().set_cell("date_deadline", day(1)),
    )
    .await
    .unwrap();
    assert_deadline(&mut tx, m1, day(1), "seed").await;
    assert_deadline(&mut tx, m2, day(3), "shifted once").await;
    assert_deadline(&mut tx, done, day(1), "done origin").await;
    assert_deadline(&mut tx, cancelled, day(5), "cancelled successor").await;
    assert_deadline(&mut tx, behind, day(9), "behind a terminal move").await;
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// (cycle 26 review): a deadline write locks the transfer of every linked move its shift
/// reaches before writing, so a writer of the other transfer waits in the root order instead of
/// crossing it.
#[tokio::test]
async fn a_deadline_shift_locks_the_transfers_it_reaches() {
    let f = fixture().await;
    let mut setup = f.h.begin().await.unwrap();
    let p1 = create_picking(
        &mut setup,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "DL/1"),
    )
    .await
    .unwrap();
    let p2 = create_picking(
        &mut setup,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "DL/2"),
    )
    .await
    .unwrap();
    let moves = create_moves(
        &mut setup,
        f.company,
        vec![
            counted_move(&f, p1, "confirmed", "1", false, false).set_cell("date_deadline", day(0)),
            counted_move(&f, p2, "confirmed", "1", false, false).set_cell("date_deadline", day(2)),
        ],
    )
    .await
    .unwrap();
    write_moves(
        &mut setup,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("move_dest_ids", id_list(&moves[1..])),
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();
    drop(setup);
    let mut a = f.h.begin().await.unwrap();
    write_moves(
        &mut a,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("date_deadline", day(1)),
    )
    .await
    .unwrap();
    assert_deadline(&mut a, moves[1], day(3), "shifted").await;
    let mut b = f.h.begin().await.unwrap();
    let error = b
        .query(
            "SELECT FROM stock_picking WHERE id = $1 FOR UPDATE NOWAIT",
            &[&p2],
        )
        .await
        .expect_err("the reached transfer was not locked");
    assert_eq!(error.sqlstate(), Some("55P03"), "{error:?}");
    b.rollback().await.unwrap();
    drop(b);
    a.rollback().await.unwrap();
    drop(a);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn deadline_values_without_a_shift_and_the_chain_bound() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let mv = |deadline: Cell| {
        with_demand(move_vals(&f, f.company), "1")
            .set_cell("state", text("confirmed"))
            .set_cell("date_deadline", deadline)
    };
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![mv(Cell::Null), mv(day(12)), mv(day(30)), mv(Cell::Null)],
    )
    .await
    .unwrap();
    for pair in moves.windows(2) {
        write_moves(
            &mut tx,
            f.company,
            &pair[..1],
            RawVals::new().set_cell("move_dest_ids", id_list(&pair[1..])),
        )
        .await
        .unwrap();
    }
    // the seed had no deadline: its neighbour takes the value, and the walk shifts from there
    write_moves(
        &mut tx,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("date_deadline", day(7)),
    )
    .await
    .unwrap();
    for (id, want) in moves.iter().zip([day(7), day(7), day(25), day(25)]) {
        assert_deadline(&mut tx, *id, want, "no shift").await;
    }
    // four linked moves, a bound of three: refused by name, nothing changed
    tx.env()
        .records("ir.config_parameter")
        .unwrap()
        .call::<(String, Option<i64>), i64>(
            &mut tx,
            "set_int",
            ("stock.move_chain_limit".to_owned(), Some(3)),
        )
        .await
        .unwrap();
    let error = write_moves(
        &mut tx,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("date_deadline", day(8)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert_eq!(
        error.message(),
        erp_addon_stock::StockError::ChainLimit { limit: 3 }.to_string()
    );
    for (id, want) in moves.iter().zip([day(7), day(7), day(25), day(25)]) {
        assert_deadline(&mut tx, *id, want, "refused").await;
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn delay_alert_ignores_terminal_origins() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let mv = |state: &str, date: Cell| {
        with_demand(move_vals(&f, f.company), "1")
            .set_cell("state", text(state))
            .set_cell("date", date)
    };
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            mv("confirmed", day(0)),
            mv("done", day(5)),
            mv("cancel", day(6)),
            mv("confirmed", day(3)),
            mv("waiting", day(4)),
        ],
    )
    .await
    .unwrap();
    let (target, done, cancelled, open3, open4) =
        (moves[0], moves[1], moves[2], moves[3], moves[4]);
    let alert = async |tx: &mut Transaction| {
        let cached = tx
            .env()
            .records("stock.move")
            .unwrap()
            .browse([target])
            .unwrap()
            .cell(tx, "delay_alert_date")
            .await
            .unwrap();
        tx.flush().await.unwrap();
        let rows = tx
            .query(
                "SELECT delay_alert_date FROM stock_move WHERE id = $1",
                &[&target],
            )
            .await
            .unwrap();
        let stored: Option<DateTime> = rows[0].get(0);
        assert_eq!(
            stored.map_or(Cell::Null, Cell::DateTime),
            cached,
            "stored and cached"
        );
        cached
    };
    let origins = |ids: &[i64]| RawVals::new().set_cell("move_orig_ids", id_list(ids));
    write_moves(&mut tx, f.company, &[target], origins(&[done, cancelled]))
        .await
        .unwrap();
    assert_eq!(
        alert(&mut tx).await,
        Cell::Null,
        "only terminal origins are later"
    );
    write_moves(
        &mut tx,
        f.company,
        &[target],
        origins(&[done, cancelled, open3, open4]),
    )
    .await
    .unwrap();
    assert_eq!(alert(&mut tx).await, day(4), "the latest unfinished origin");
    write_moves(
        &mut tx,
        f.company,
        &[open4],
        RawVals::new().set_cell("date", day(-1)),
    )
    .await
    .unwrap();
    assert_eq!(alert(&mut tx).await, day(3), "an origin's date change");
    write_moves(
        &mut tx,
        f.company,
        &[open3],
        RawVals::new().set_cell("state", text("cancel")),
    )
    .await
    .unwrap();
    assert_eq!(alert(&mut tx).await, Cell::Null, "an origin's state change");
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

// ---- the unit snapshot of a move (6) ----

/// A unit of `factor` fixture units (the fixture's reference unit is its root).
async fn derived_unit(tx: &mut Transaction, f: &Fixture, name: &str, factor: &str) -> i64 {
    create(
        tx,
        "uom.uom",
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("relative_factor", dec(factor))
            .set_cell("relative_uom_id", Cell::Id(f.unit)),
    )
    .await
}

/// The product-unit quantity of `qty` in `unit` by the unit service's pure conversion, with the
/// Product Unit digits configured now.
async fn expected_product_qty(
    tx: &mut Transaction,
    unit: i64,
    product_unit: i64,
    qty: &str,
) -> Cell {
    let env = tx.env();
    let line = erp_addon_uom::models::load_unit(&env, tx, unit)
        .await
        .unwrap();
    let product = erp_addon_uom::models::load_unit(&env, tx, product_unit)
        .await
        .unwrap();
    let digits = erp_addon_base::decimal_precision::precision_get(&env, tx, "Product Unit")
        .await
        .unwrap();
    let preview = ProductUnitPreview::new(&line, &product, digits).unwrap();
    Cell::Decimal(preview.product_quantity(qty.parse().unwrap()).unwrap())
}

async fn move_cells(tx: &mut Transaction, id: i64, fields: &[&str]) -> Vec<Cell> {
    let records = tx
        .env()
        .records("stock.move")
        .unwrap()
        .browse([id])
        .unwrap();
    let mut out = Vec::new();
    for field in fields {
        out.push(records.cell(tx, field).await.unwrap());
    }
    out
}

/// The values of a move of the fixture's company without unit, demand or snapshot.
fn bare_move(f: &Fixture) -> RawVals {
    RawVals::new()
        .set_cell("company_id", Cell::Id(f.company))
        .set_cell("product_id", Cell::Id(f.product))
        .set_cell("location_id", Cell::Id(f.location))
        .set_cell("location_dest_id", Cell::Id(f.other_location))
}

#[tokio::test]
async fn a_bare_move_takes_draft_zero_demand_and_the_product_unit() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let id = create_move(&mut tx, f.company, bare_move(&f))
        .await
        .unwrap();
    let product_unit = tx
        .env()
        .records("product.product")
        .unwrap()
        .browse([f.product])
        .unwrap()
        .cell(&mut tx, "uom_id")
        .await
        .unwrap();
    let cells = move_cells(
        &mut tx,
        id,
        &[
            "state",
            "product_uom_qty",
            "uom_id",
            "product_qty",
            "quantity_product_uom",
        ],
    )
    .await;
    assert_eq!(cells[0], text("draft"));
    assert_eq!(cells[1], dec("0"));
    assert_eq!(cells[2], product_unit, "the product's unit");
    let zero = expected_product_qty(&mut tx, f.unit, f.unit, "0").await;
    assert_eq!(cells[3], zero);
    assert_eq!(cells[4], zero);
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn demand_and_quantity_convert_to_the_product_unit() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let pack = derived_unit(&mut tx, &f, "Pack of 6", "6").await;
    let third = derived_unit(&mut tx, &f, "Third", "0.333333").await;
    // demands within the product-unit precision (two digits); a third of a unit rounds on conversion
    for (unit, qty) in [(pack, "2"), (third, "0.05"), (pack, "-1.5")] {
        let id = create_move(
            &mut tx,
            f.company,
            bare_move(&f)
                .set_cell("uom_id", Cell::Id(unit))
                .set_cell("product_uom_qty", dec(qty)),
        )
        .await
        .unwrap();
        // a new move has no quantity: it follows from the lines (move_flow tests)
        let want = expected_product_qty(&mut tx, unit, f.unit, qty).await;
        let zero = expected_product_qty(&mut tx, unit, f.unit, "0").await;
        let cells = move_cells(&mut tx, id, &["product_qty", "quantity_product_uom"]).await;
        assert_eq!(cells, vec![want, zero], "{qty} of unit {unit}");
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn writes_recompute_the_snapshot_and_keep_the_demand() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let pack = derived_unit(&mut tx, &f, "Pack of 6", "6").await;
    let template = tx
        .env()
        .with_company(f.company)
        .unwrap()
        .records("product.template")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("name", text("Gadget"))],
        )
        .await
        .unwrap()
        .ids()[0];
    let other: i64 = tx
        .query(
            "SELECT id FROM product_product WHERE product_tmpl_id = $1",
            &[&template],
        )
        .await
        .unwrap()[0]
        .get(0);
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            bare_move(&f)
                .set_cell("uom_id", Cell::Id(pack))
                .set_cell("product_uom_qty", dec("100")),
            bare_move(&f).set_cell("product_uom_qty", dec("4")),
        ],
    )
    .await
    .unwrap();
    // a product change keeps the demand and takes the new product's unit
    write_moves(
        &mut tx,
        f.company,
        &moves[..1],
        RawVals::new().set_cell("product_id", Cell::Id(other)),
    )
    .await
    .unwrap();
    let cells = move_cells(
        &mut tx,
        moves[0],
        &["product_uom_qty", "uom_id", "product_qty"],
    )
    .await;
    assert_eq!(cells[0], dec("100"), "demand kept");
    assert_eq!(cells[1], Cell::Id(f.unit), "the new product's unit");
    assert_eq!(
        cells[2],
        expected_product_qty(&mut tx, f.unit, f.unit, "100").await
    );
    // one demand written on moves of two units: each converts with its own unit
    write_moves(
        &mut tx,
        f.company,
        &[moves[1]],
        RawVals::new().set_cell("uom_id", Cell::Id(pack)),
    )
    .await
    .unwrap();
    write_moves(
        &mut tx,
        f.company,
        &moves,
        RawVals::new().set_cell("product_uom_qty", dec("3")),
    )
    .await
    .unwrap();
    tx.flush().await.unwrap();
    for (id, unit) in [(moves[0], f.unit), (moves[1], pack)] {
        let want = expected_product_qty(&mut tx, unit, f.unit, "3").await;
        assert_eq!(
            move_cells(&mut tx, id, &["product_qty"]).await,
            vec![want],
            "move {id}"
        );
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_foreign_snapshot_value_is_refused_by_name() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let label = |tx: &Transaction, field: &str| {
        tx.registry()
            .model("stock.move")
            .unwrap()
            .field(field)
            .unwrap()
            .string
            .to_string()
    };
    let error = create_move(
        &mut tx,
        f.company,
        bare_move(&f).set_cell("uom_factor", dec("7")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming, "{error:?}");
    assert_eq!(
        error.message(),
        erp_addon_stock::StockError::UnitSnapshot {
            field: label(&tx, "uom_factor")
        }
        .to_string()
    );
    let id = create_move(&mut tx, f.company, bare_move(&f))
        .await
        .unwrap();
    let error = write_moves(
        &mut tx,
        f.company,
        &[id],
        RawVals::new().set_cell("product_qty", dec("5")),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.message(),
        erp_addon_stock::StockError::UnitSnapshot {
            field: label(&tx, "product_qty")
        }
        .to_string()
    );
    // the value the unit service gives is accepted
    let same = expected_product_qty(&mut tx, f.unit, f.unit, "0").await;
    write_moves(
        &mut tx,
        f.company,
        &[id],
        RawVals::new().set_cell("product_qty", same),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// The generic upsert by the declared reference key is refused for transfers: it would run none of
/// the create and write checks, the derived state and counters among them.
#[tokio::test]
async fn transfer_upsert_is_refused() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let pickings = tx
        .env()
        .with_company(f.company)
        .unwrap()
        .records("stock.picking")
        .unwrap();
    let row = picking_vals(&f, f.company, f.picking_type, "UPS/1").set_cell("state", text("done"));
    let error = tx
        .savepoint(async |tx| {
            pickings
                .upsert(
                    tx,
                    erp_sdk::UpsertCall::new("_name_uniq")
                        .with_rows([row])
                        .with_update(["state"]),
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming, "{error:?}");
    assert_eq!(
        error.message(),
        erp_addon_stock::StockError::TransferUpsert.to_string()
    );
    let stored: i64 = tx
        .query(
            "SELECT count(*) FROM stock_picking WHERE name = 'UPS/1'",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(stored, 0);
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// A transfer created or re-sourced by a transaction whose snapshot predates a committed usage
/// change of its source location is retried (`40001`): it is not stored with a state evaluated
/// under the old usage. On a fresh snapshot the same create passes.
#[tokio::test]
async fn a_transfer_behind_a_source_usage_change_is_retried() {
    let f = fixture().await;
    let mut setup = f.h.begin().await.unwrap();
    let mut sources = Vec::new();
    for name in ["Behind A", "Behind B"] {
        sources.push(
            create(
                &mut setup,
                "stock.location",
                RawVals::new().set_cell("name", text(name)),
            )
            .await,
        );
    }
    let existing = create_picking(
        &mut setup,
        f.company,
        picking_vals(&f, f.company, f.picking_type, "SRC/1"),
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();
    drop(setup);
    let mut behind = f.h.begin().await.unwrap();
    behind.query("SELECT 1", &[]).await.unwrap();
    let mut change = f.h.begin().await.unwrap();
    change
        .execute(
            "UPDATE stock_location SET usage = 'supplier' WHERE id = ANY($1)",
            &[&sources],
        )
        .await
        .unwrap();
    change.commit().await.unwrap();
    drop(change);
    let from = |name: &str, source: i64| {
        picking_vals(&f, f.company, f.picking_type, name).set_cell("location_id", Cell::Id(source))
    };
    let error = create_picking(&mut behind, f.company, from("SRC/2", sources[0]))
        .await
        .expect_err("a transfer was created under a usage older than the committed one");
    assert_eq!(error.sqlstate(), Some("40001"), "{error:?}");
    let error = behind
        .savepoint(async |tx| {
            tx.env()
                .with_company(f.company)?
                .records("stock.picking")?
                .browse([existing])?
                .write(
                    tx,
                    RawVals::new().set_cell("location_id", Cell::Id(sources[1])),
                )
                .await
        })
        .await
        .expect_err("a transfer was re-sourced under a usage older than the committed one");
    assert_eq!(error.sqlstate(), Some("40001"), "{error:?}");
    behind.rollback().await.unwrap();
    drop(behind);
    let mut tx = f.h.begin().await.unwrap();
    create_picking(&mut tx, f.company, from("SRC/2", sources[0]))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// Moves that keep their unit, product and stored factors share one write group whatever their
/// other amount: a quantity written on moves of three demands is converted once per unit and each
/// demand's product-unit amount stays as stored.
#[tokio::test]
async fn an_amount_write_groups_moves_by_unit_not_by_their_other_amount() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let pack = derived_unit(&mut tx, &f, "Pack of 6", "6").await;
    let mv = |unit: i64, qty: &str| {
        bare_move(&f)
            .set_cell("uom_id", Cell::Id(unit))
            .set_cell("product_uom_qty", dec(qty))
    };
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            mv(f.unit, "1"),
            mv(f.unit, "2"),
            mv(pack, "3"),
            mv(f.unit, "4"),
        ],
    )
    .await
    .unwrap();
    let records = tx
        .env()
        .with_company(f.company)
        .unwrap()
        .records("stock.move")
        .unwrap()
        .browse(moves.iter().copied())
        .unwrap();
    let vals = RawVals::new().set_cell("quantity", dec("2"));
    let groups = erp_addon_stock::move_units::write_groups(&records, &mut tx, &vals)
        .await
        .unwrap();
    let grouped: Vec<Vec<i64>> = groups.iter().map(|(ids, _)| ids.clone()).collect();
    assert_eq!(
        grouped,
        vec![vec![moves[0], moves[1], moves[3]], vec![moves[2]]]
    );
    for ((_, group), unit) in groups.iter().zip([f.unit, pack]) {
        let want = expected_product_qty(&mut tx, unit, f.unit, "2").await;
        assert_eq!(
            group.get("quantity_product_uom"),
            Some(&want),
            "unit {unit}"
        );
        assert!(
            !group.contains("product_qty"),
            "unit {unit}: the demand's product-unit amount is written again"
        );
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}

/// A demand and a deadline written together on moves of two units: each unit converts its own
/// demand, and the linked moves are shifted from one plan for the whole write, as a write on moves
/// of one unit shifts them. The move behind the second seed moves by the days that seed moved.
#[tokio::test]
async fn a_deadline_written_with_a_demand_over_two_units_shifts_from_one_plan() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let pack = derived_unit(&mut tx, &f, "Pack of 6", "6").await;
    let open = |unit: i64, deadline: Cell| {
        bare_move(&f)
            .set_cell("uom_id", Cell::Id(unit))
            .set_cell("product_uom_qty", dec("1"))
            .set_cell("state", text("confirmed"))
            .set_cell("date_deadline", deadline)
    };
    let moves = create_moves(
        &mut tx,
        f.company,
        vec![
            open(f.unit, day(0)),
            open(pack, day(0)),
            open(f.unit, day(5)),
        ],
    )
    .await
    .unwrap();
    let (first, second, behind) = (moves[0], moves[1], moves[2]);
    for (orig, dest) in [(first, second), (second, behind)] {
        write_moves(
            &mut tx,
            f.company,
            &[orig],
            RawVals::new().set_cell("move_dest_ids", id_list(&[dest])),
        )
        .await
        .unwrap();
    }
    write_moves(
        &mut tx,
        f.company,
        &[first, second],
        RawVals::new()
            .set_cell("product_uom_qty", dec("3"))
            .set_cell("date_deadline", day(2)),
    )
    .await
    .unwrap();
    assert_deadline(&mut tx, first, day(2), "first seed").await;
    assert_deadline(&mut tx, second, day(2), "second seed").await;
    assert_deadline(&mut tx, behind, day(7), "behind the second seed").await;
    for (id, unit) in [(first, f.unit), (second, pack)] {
        let want = expected_product_qty(&mut tx, unit, f.unit, "3").await;
        assert_eq!(
            move_cells(&mut tx, id, &["product_qty"]).await,
            vec![want],
            "move {id}"
        );
    }
    tx.rollback().await.unwrap();
    drop(tx);
    f.h.cleanup().await.unwrap();
}
