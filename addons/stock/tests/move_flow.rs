// SPDX-License-Identifier: MIT OR Apache-2.0
//! Move lines, reservation, confirmation and validation of transfers: open lines take their move's
//! defaults, hold their reservation on their quant key and keep their move's quantity and state;
//! validation moves the stock once and fixes the done work. Every
//! compared value is read back from the database or through the quant engine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use erp_addon_stock::{QuantChange, QuantKey, apply_quant_changes};
use erp_sdk::ErrorKind;
use erp_sdk::prelude::*;
use erp_testkit::Harness;

erp_sdk::addon!(name = "test_stock_move_flow", depends = ["stock"]);

/// A test stand-in for `ir.model.data` (the real model lives in `erp-data`); the same as the other
/// stock tests.
#[model(
    name = "ir.model.data",
    description = "Model Data",
    addon = "test_stock_move_flow"
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
    #[field(boolean)]
    pub noupdate: bool,
}

/// A company with a storable product P, a consumable C, units and dozens, an internal stock
/// location L, a vendor location V, a customer location X, a receipt type V to L, a delivery
/// type L to X and a delivery type L to X that does not reserve at confirmation.
struct Fixture {
    h: Harness,
    company: i64,
    p: i64,
    consumable: i64,
    unit: i64,
    dozen: i64,
    l: i64,
    vendor: i64,
    customer: i64,
    receipt_type: i64,
    delivery_type: i64,
    manual_type: i64,
}

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

fn dec(value: &str) -> Decimal {
    Decimal::from_str_exact(value).unwrap()
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

async fn product(tx: &mut Transaction, company: i64, name: &str, storable: bool) -> i64 {
    let env = tx.env().with_company(company).unwrap();
    let template = env
        .records("product.template")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", text(name))
                    .set_cell("is_storable", Cell::Bool(storable)),
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

async fn location(tx: &mut Transaction, name: &str, usage: &str) -> i64 {
    create(
        tx,
        "stock.location",
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("usage", text(usage)),
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
    let dozen = create(
        &mut tx,
        "uom.uom",
        RawVals::new()
            .set_cell("name", text("Dozens"))
            .set_cell("relative_factor", Cell::Decimal(dec("12")))
            .set_cell("relative_uom_id", Cell::Id(unit)),
    )
    .await;
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
    let company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Main Company"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let uid = tx.env().uid();
    tx.execute(
        "INSERT INTO res_company_users_rel (user_id, cid) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        &[&uid, &company],
    )
    .await
    .unwrap();
    let p = product(&mut tx, company, "P", true).await;
    let consumable = product(&mut tx, company, "C", false).await;
    let l = location(&mut tx, "L", "internal").await;
    let vendor = location(&mut tx, "Vendors", "supplier").await;
    let customer = location(&mut tx, "Customers", "customer").await;
    let mut types = Vec::new();
    for (name, code, src, dest, method) in [
        ("Receipts", "incoming", vendor, l, "at_confirm"),
        ("Deliveries", "outgoing", l, customer, "at_confirm"),
        ("Manual deliveries", "outgoing", l, customer, "manual"),
    ] {
        types.push(
            create(
                &mut tx,
                "stock.picking.type",
                RawVals::new()
                    .set_cell("name", text(name))
                    .set_cell("code", text(code))
                    .set_cell("company_id", Cell::Id(company))
                    .set_cell("default_location_src_id", Cell::Id(src))
                    .set_cell("default_location_dest_id", Cell::Id(dest))
                    .set_cell("reservation_method", text(method)),
            )
            .await,
        );
    }
    tx.commit().await.unwrap();
    Fixture {
        h,
        company,
        p,
        consumable,
        unit,
        dozen,
        l,
        vendor,
        customer,
        receipt_type: types[0],
        delivery_type: types[1],
        manual_type: types[2],
    }
}

impl Fixture {
    fn env(&self, tx: &Transaction) -> Env {
        tx.env().with_company(self.company).unwrap()
    }

    /// Put `qty` of `product` on hand at L.
    async fn stock(&self, tx: &mut Transaction, product: i64, qty: &str) {
        let key = QuantKey::new(self.company, product, self.l);
        apply_quant_changes(tx, &[QuantChange::new(key, dec(qty), Decimal::ZERO)])
            .await
            .unwrap();
    }

    /// A transfer of `picking_type` from `src` to `dest`, with a reference unique in the run.
    async fn picking(&self, tx: &mut Transaction, picking_type: i64, src: i64, dest: i64) -> i64 {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let env = self.env(tx);
        env.records("stock.picking")
            .unwrap()
            .create(
                tx,
                vec![
                    RawVals::new()
                        .set_cell("name", text(&format!("T{picking_type}-{src}-{dest}-{n}")))
                        .set_cell("company_id", Cell::Id(self.company))
                        .set_cell("picking_type_id", Cell::Id(picking_type))
                        .set_cell("location_id", Cell::Id(src))
                        .set_cell("location_dest_id", Cell::Id(dest)),
                ],
            )
            .await
            .unwrap()
            .rec_ids()[0]
            .get()
    }

    /// A move of `product` in `picking` (or none) from `src` to `dest` with demand `qty` units.
    async fn move_in(
        &self,
        tx: &mut Transaction,
        picking: Option<i64>,
        product: i64,
        src: i64,
        dest: i64,
        qty: &str,
    ) -> i64 {
        let mut vals = RawVals::new()
            .set_cell("company_id", Cell::Id(self.company))
            .set_cell("product_id", Cell::Id(product))
            .set_cell("location_id", Cell::Id(src))
            .set_cell("location_dest_id", Cell::Id(dest))
            .set_cell("product_uom_qty", Cell::Decimal(dec(qty)));
        if let Some(picking) = picking {
            vals = vals.set_cell("picking_id", Cell::Id(picking));
        }
        let env = self.env(tx);
        env.records("stock.move")
            .unwrap()
            .create(tx, vec![vals])
            .await
            .unwrap()
            .ids()[0]
    }

    /// Confirm the draft moves of `picking` through the transfer's Confirm.
    async fn confirm(&self, tx: &mut Transaction, picking: i64) {
        self.call(tx, picking, "action_confirm").await.unwrap();
    }

    async fn add_lines(&self, tx: &mut Transaction, vals: Vec<RawVals>) -> Result<Vec<i64>> {
        let env = self.env(tx);
        tx.savepoint(async |tx| {
            Ok(env
                .records("stock.move.line")?
                .create(tx, vals)
                .await?
                .ids())
        })
        .await
    }

    async fn write_lines(&self, tx: &mut Transaction, ids: &[i64], vals: RawVals) -> Result<()> {
        let env = self.env(tx);
        tx.savepoint(async |tx| {
            env.records("stock.move.line")?
                .browse(ids.iter().copied())?
                .write(tx, vals)
                .await
        })
        .await
    }

    async fn unlink_lines(&self, tx: &mut Transaction, ids: &[i64]) -> Result<()> {
        let env = self.env(tx);
        tx.savepoint(async |tx| {
            env.records("stock.move.line")?
                .browse(ids.iter().copied())?
                .unlink(tx)
                .await
        })
        .await
    }

    /// `(on hand, reserved)` of P-like `product` at L, `None` when no row is stored.
    async fn quant(&self, tx: &mut Transaction, product: i64) -> Option<(Decimal, Decimal)> {
        tx.query(
            "SELECT quantity, reserved_quantity FROM stock_quant \
             WHERE product_id = $1 AND location_id = $2 AND lot_id IS NULL \
               AND package_id IS NULL AND owner_id IS NULL",
            &[&product, &self.l],
        )
        .await
        .unwrap()
        .first()
        .map(|row| (row.get(0), row.get(1)))
    }

    /// Number of stored quant rows of `product` at any location.
    async fn quant_rows(&self, tx: &mut Transaction, product: i64) -> i64 {
        tx.query(
            "SELECT count(*) FROM stock_quant WHERE product_id = $1",
            &[&product],
        )
        .await
        .unwrap()[0]
            .get(0)
    }
}

fn line(move_id: i64, qty: &str) -> RawVals {
    RawVals::new()
        .set_cell("move_id", Cell::Id(move_id))
        .set_cell("quantity", Cell::Decimal(dec(qty)))
}

/// `(quantity, quantity in product unit, state, open line version)` of a move, from the table.
async fn move_row(tx: &mut Transaction, id: i64) -> (Decimal, Decimal, String, i32) {
    let row = tx
        .query(
            "SELECT quantity, quantity_product_uom, state, open_line_version FROM stock_move \
             WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .remove(0);
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

async fn picking_state(tx: &mut Transaction, id: i64) -> String {
    tx.query("SELECT state FROM stock_picking WHERE id = $1", &[&id])
        .await
        .unwrap()[0]
        .get(0)
}

fn user_error(error: &Error, needle: &str) {
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert!(error.to_string().contains(needle), "{error}");
}

#[tokio::test]
async fn a_line_takes_its_moves_defaults() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "5")
        .await;
    let env = f.env(&tx);
    env.records("stock.move")
        .unwrap()
        .browse([mv])
        .unwrap()
        .write(&mut tx, RawVals::new().set_cell("picked", Cell::Bool(true)))
        .await
        .unwrap();
    let ids = f.add_lines(&mut tx, vec![line(mv, "2")]).await.unwrap();
    let row = tx
        .query(
            "SELECT picking_id, company_id, product_id, location_id, location_dest_id, uom_id, \
             picked, quantity_product_uom FROM stock_move_line WHERE id = $1",
            &[&ids[0]],
        )
        .await
        .unwrap()
        .remove(0);
    assert_eq!(row.get::<_, Option<i64>>(0), Some(picking));
    assert_eq!(row.get::<_, i64>(1), f.company);
    assert_eq!(row.get::<_, i64>(2), f.p);
    assert_eq!(row.get::<_, i64>(3), f.l);
    assert_eq!(row.get::<_, i64>(4), f.customer);
    assert_eq!(row.get::<_, i64>(5), f.unit);
    assert!(row.get::<_, bool>(6));
    assert_eq!(row.get::<_, Decimal>(7), dec("2"));
}

#[tokio::test]
async fn move_quantity_is_the_sum_of_its_lines_in_the_move_unit() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.receipt_type, f.vendor, f.l).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.vendor, f.l, "30")
        .await;
    let ids = f
        .add_lines(
            &mut tx,
            vec![
                line(mv, "3"),
                line(mv, "1").set_cell("uom_id", Cell::Id(f.dozen)),
            ],
        )
        .await
        .unwrap();
    let (quantity, product_quantity, _, version) = move_row(&mut tx, mv).await;
    assert_eq!(quantity, dec("15"));
    assert_eq!(product_quantity, dec("15"));
    assert_eq!(version, 1);
    // a receipt from a vendor reserves nothing and stores no balance
    assert_eq!(f.quant_rows(&mut tx, f.p).await, 0);
    f.unlink_lines(&mut tx, &ids).await.unwrap();
    let (quantity, product_quantity, _, version) = move_row(&mut tx, mv).await;
    assert_eq!(quantity, Decimal::ZERO);
    assert_eq!(product_quantity, Decimal::ZERO);
    assert_eq!(version, 2);
}

/// reservation slice's)
#[tokio::test]
async fn an_open_line_holds_its_reservation_until_it_is_deleted() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.manual_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "10")
        .await;
    f.confirm(&mut tx, picking).await;
    let ids = f.add_lines(&mut tx, vec![line(mv, "4")]).await.unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("4"))));
    let (_, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!(state, "partially_available");
    // an explicit key may take more than is free where negative stock is allowed
    f.write_lines(
        &mut tx,
        &ids,
        RawVals::new().set_cell("quantity", Cell::Decimal(dec("15"))),
    )
    .await
    .unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("15"))));
    let (quantity, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!((quantity, state.as_str()), (dec("15"), "assigned"));
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
    f.unlink_lines(&mut tx, &ids).await.unwrap();
    assert_eq!(
        f.quant(&mut tx, f.p).await,
        Some((dec("10"), Decimal::ZERO))
    );
    let (quantity, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!((quantity, state.as_str()), (Decimal::ZERO, "confirmed"));
    assert_eq!(picking_state(&mut tx, picking).await, "confirmed");
}

#[tokio::test]
async fn a_key_edit_moves_the_reservation_to_the_new_key() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    let other = location(&mut tx, "L2", "internal").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.manual_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "5")
        .await;
    f.confirm(&mut tx, picking).await;
    let ids = f.add_lines(&mut tx, vec![line(mv, "5")]).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    f.write_lines(
        &mut tx,
        &ids,
        RawVals::new().set_cell("location_id", Cell::Id(other)),
    )
    .await
    .unwrap();
    assert_eq!(
        f.quant(&mut tx, f.p).await,
        Some((dec("10"), Decimal::ZERO))
    );
    let reserved: Decimal = tx
        .query(
            "SELECT reserved_quantity FROM stock_quant WHERE product_id = $1 AND location_id = $2",
            &[&f.p, &other],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(reserved, dec("5"));
}

/// balance)
#[tokio::test]
async fn bypass_lines_store_no_reservation() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.manual_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.vendor, f.l, "3")
        .await;
    let consumable = f
        .move_in(&mut tx, Some(picking), f.consumable, f.l, f.customer, "3")
        .await;
    // both bypass the reservation, so confirming reserves them at once without a balance
    f.confirm(&mut tx, picking).await;
    assert_eq!(lines_of(&mut tx, mv).await, vec![(dec("3"), f.vendor)]);
    assert_eq!(f.quant_rows(&mut tx, f.p).await, 0);
    assert_eq!(f.quant_rows(&mut tx, f.consumable).await, 0);
    let (_, _, state, _) = move_row(&mut tx, consumable).await;
    assert_eq!(state, "assigned");
}

#[tokio::test]
async fn line_refusals_are_named() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let mv = f.move_in(&mut tx, None, f.p, f.vendor, f.l, "3").await;
    let other = f
        .move_in(&mut tx, None, f.consumable, f.vendor, f.l, "3")
        .await;
    let ids = f.add_lines(&mut tx, vec![line(mv, "1")]).await.unwrap();

    let error = f
        .add_lines(
            &mut tx,
            vec![line(mv, "1").set_cell("lot_name", text("A1"))],
        )
        .await
        .unwrap_err();
    user_error(&error, "a later release");
    let error = f
        .add_lines(
            &mut tx,
            vec![RawVals::new().set_cell("quantity", Cell::Decimal(dec("1")))],
        )
        .await
        .unwrap_err();
    user_error(&error, "add the product to the transfer");
    let error = f
        .write_lines(
            &mut tx,
            &ids,
            RawVals::new().set_cell("move_id", Cell::Id(other)),
        )
        .await
        .unwrap_err();
    user_error(&error, "follows its stock move");
    let error = f
        .write_lines(
            &mut tx,
            &ids,
            RawVals::new().set_cell("quantity", Cell::Decimal(dec("-1"))),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("Quantity cannot be negative"),
        "{error}"
    );

    // a line in the ledger is neither changed nor deleted here
    tx.execute(
        "UPDATE stock_move_line SET membership = 'done', done_at = now() WHERE id = $1",
        &[&ids[0]],
    )
    .await
    .unwrap();
    tx.invalidate_model("stock.move.line", None).await.unwrap();
    let error = f
        .write_lines(
            &mut tx,
            &ids,
            RawVals::new().set_cell("quantity", Cell::Decimal(dec("2"))),
        )
        .await
        .unwrap_err();
    user_error(&error, "done operation line");
    let error = f.unlink_lines(&mut tx, &ids).await.unwrap_err();
    user_error(&error, "done operation line");

    tx.execute(
        "UPDATE stock_move SET state = 'cancel' WHERE id = $1",
        &[&other],
    )
    .await
    .unwrap();
    tx.invalidate_model("stock.move", None).await.unwrap();
    let error = f
        .add_lines(&mut tx, vec![line(other, "1")])
        .await
        .unwrap_err();
    user_error(&error, "done or cancelled move");
}

impl Fixture {
    async fn call(&self, tx: &mut Transaction, picking: i64, method: &'static str) -> Result<()> {
        let records = self.env(tx).records("stock.picking")?.browse([picking])?;
        tx.savepoint(async move |tx| records.call::<(), ()>(tx, method, ()).await)
            .await
    }

    /// A delivery transfer from L of the manual type with one confirmed, unreserved move of `qty`
    /// of `product`.
    async fn delivery(&self, tx: &mut Transaction, product: i64, qty: &str) -> (i64, i64) {
        let picking = self
            .picking(tx, self.manual_type, self.l, self.customer)
            .await;
        let mv = self
            .move_in(tx, Some(picking), product, self.l, self.customer, qty)
            .await;
        self.confirm(tx, picking).await;
        (picking, mv)
    }
}

/// `(quantity, location)` of each line of a move, by id.
async fn lines_of(tx: &mut Transaction, mv: i64) -> Vec<(Decimal, i64)> {
    tx.query(
        "SELECT quantity, location_id FROM stock_move_line WHERE move_id = $1 ORDER BY id",
        &[&mv],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1)))
    .collect()
}

#[tokio::test]
async fn check_availability_reserves_the_full_demand() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.delivery(&mut tx, f.p, "10").await;
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(lines_of(&mut tx, mv).await, vec![(dec("10"), f.l)]);
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("10"))));
    let (quantity, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!((quantity, state.as_str()), (dec("10"), "assigned"));
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
}

/// one)
#[tokio::test]
async fn partial_and_empty_availability_and_a_later_top_up() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let (empty_picking, empty) = f.delivery(&mut tx, f.p, "3").await;
    f.call(&mut tx, empty_picking, "action_assign")
        .await
        .unwrap();
    assert!(lines_of(&mut tx, empty).await.is_empty());
    let (_, _, state, _) = move_row(&mut tx, empty).await;
    assert_eq!(state, "confirmed");
    assert_eq!(f.quant_rows(&mut tx, f.p).await, 0);
    tx.rollback().await.unwrap();

    // shortcut: stock and transfer share one transaction because a fifth transaction here timed
    // out waiting for a pool connection (pool of 4, cause not found); split it again once fixed
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "4").await;
    let (picking, mv) = f.delivery(&mut tx, f.p, "10").await;
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(lines_of(&mut tx, mv).await, vec![(dec("4"), f.l)]);
    let (_, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!(state, "partially_available");
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
    tx.commit().await.unwrap();

    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "6").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(lines_of(&mut tx, mv).await, vec![(dec("10"), f.l)]);
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("10"))));
    let (_, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!(state, "assigned");
}

#[tokio::test]
async fn a_receipt_from_a_vendor_is_ready_without_stock() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.receipt_type, f.vendor, f.l).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.vendor, f.l, "7")
        .await;
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(lines_of(&mut tx, mv).await, vec![(dec("7"), f.vendor)]);
    assert_eq!(f.quant_rows(&mut tx, f.p).await, 0);
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
}

#[tokio::test]
async fn reservation_in_another_unit_never_takes_more_than_is_free() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "30").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.manual_type, f.l, f.customer).await;
    let env = f.env(&tx);
    let mv = env
        .records("stock.move")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("company_id", Cell::Id(f.company))
                    .set_cell("picking_id", Cell::Id(picking))
                    .set_cell("product_id", Cell::Id(f.p))
                    .set_cell("uom_id", Cell::Id(f.dozen))
                    .set_cell("location_id", Cell::Id(f.l))
                    .set_cell("location_dest_id", Cell::Id(f.customer))
                    .set_cell("product_uom_qty", Cell::Decimal(dec("3"))),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    // check availability confirms the draft transfer first
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    let reserved = f.quant(&mut tx, f.p).await.unwrap().1;
    assert!(reserved <= dec("30"), "{reserved}");
    let (quantity, product_quantity, state, _) = move_row(&mut tx, mv).await;
    assert_eq!(product_quantity, reserved);
    assert!(quantity < dec("3"), "{quantity}");
    assert_eq!(state, "partially_available");
}

#[tokio::test]
async fn unreserve_releases_unpicked_lines_only() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.delivery(&mut tx, f.p, "10").await;
    let ids = f
        .add_lines(
            &mut tx,
            vec![
                line(mv, "6"),
                line(mv, "3").set_cell("picked", Cell::Bool(true)),
            ],
        )
        .await
        .unwrap();
    f.call(&mut tx, picking, "do_unreserve").await.unwrap();
    let left: Vec<i64> = tx
        .query("SELECT id FROM stock_move_line WHERE move_id = $1", &[&mv])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(left, vec![ids[1]]);
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("3"))));
    let (quantity, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!(
        (quantity, state.as_str()),
        (dec("3"), "partially_available")
    );

    tx.execute("UPDATE stock_move SET state = 'done' WHERE id = $1", &[&mv])
        .await
        .unwrap();
    tx.invalidate_model("stock.move", None).await.unwrap();
    let error = f.call(&mut tx, picking, "do_unreserve").await.unwrap_err();
    user_error(&error, "cannot be unreserved");
}

/// A transfer without moves has nothing to check the availability for.
#[tokio::test]
async fn check_availability_needs_an_open_move() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let error = f.call(&mut tx, picking, "action_assign").await.unwrap_err();
    user_error(&error, "nothing to check the availability for");
}

#[tokio::test]
async fn a_flagged_location_floors_every_reservation() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "5").await;
    tx.execute(
        "UPDATE stock_location_policy SET forbid_negative = true WHERE location_id = $1",
        &[&f.l],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (_, mv) = f.delivery(&mut tx, f.p, "8").await;
    let error = f.add_lines(&mut tx, vec![line(mv, "8")]).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert!(error.to_string().contains("only 5"), "{error}");
    assert!(error.to_string().contains("8 was requested"), "{error}");
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), Decimal::ZERO)));
    // an allocation takes what is free and stays partial
    let picking: i64 = tx
        .query("SELECT picking_id FROM stock_move WHERE id = $1", &[&mv])
        .await
        .unwrap()[0]
        .get(0);
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), dec("5"))));
}

#[tokio::test]
async fn concurrent_reservations_on_one_key_never_double_allocate() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    let (first_picking, first) = f.delivery(&mut tx, f.p, "10").await;
    let (second_picking, second) = f.delivery(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();

    // the second session's snapshot is taken before the first one reserves
    let mut late = f.h.begin().await.unwrap();
    assert_eq!(
        f.quant(&mut late, f.p).await,
        Some((dec("10"), Decimal::ZERO))
    );
    let mut tx = f.h.begin().await.unwrap();
    f.call(&mut tx, first_picking, "action_assign")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let error = f
        .call(&mut late, second_picking, "action_assign")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Concurrency, "{error:?}");
    assert_eq!(error.sqlstate(), Some("40001"), "{error:?}");
    late.rollback().await.unwrap();

    // the retry probes again and finds nothing free
    let mut tx = f.h.begin().await.unwrap();
    f.call(&mut tx, second_picking, "action_assign")
        .await
        .unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("10"))));
    assert_eq!(move_row(&mut tx, first).await.2, "assigned");
    assert_eq!(move_row(&mut tx, second).await.2, "confirmed");
    assert_eq!(f.quant_rows(&mut tx, f.p).await, 1);
}

async fn move_fields(tx: &mut Transaction, id: i64, fields: &[&str]) -> Vec<Option<i64>> {
    let mut out = Vec::new();
    for field in fields {
        let row = tx
            .query(
                &format!("SELECT {field} FROM stock_move WHERE id = $1"),
                &[&id],
            )
            .await
            .unwrap()
            .remove(0);
        out.push(row.get(0));
    }
    out
}

#[tokio::test]
async fn a_move_added_to_a_transfer_takes_its_defaults() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let partner = create(
        &mut tx,
        "res.partner",
        RawVals::new().set_cell("name", text("Customer A")),
    )
    .await;
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let env = f.env(&tx);
    env.records("stock.picking")
        .unwrap()
        .browse([picking])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("partner_id", Cell::Id(partner)),
        )
        .await
        .unwrap();
    // what the transfer form sends for a new operation row: the product and the demand
    env.records("stock.picking")
        .unwrap()
        .browse([picking])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_commands(
                "move_ids",
                vec![Command::Create(
                    RawVals::new()
                        .set_cell("product_id", Cell::Id(f.p))
                        .set_cell("product_uom_qty", Cell::Decimal(dec("2"))),
                )],
            ),
        )
        .await
        .unwrap();
    let mv: i64 = tx
        .query(
            "SELECT id FROM stock_move WHERE picking_id = $1",
            &[&picking],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        move_fields(
            &mut tx,
            mv,
            &[
                "company_id",
                "location_id",
                "location_dest_id",
                "picking_type_id",
                "partner_id",
                "uom_id"
            ]
        )
        .await,
        vec![
            Some(f.company),
            Some(f.l),
            Some(f.customer),
            Some(f.delivery_type),
            Some(partner),
            Some(f.unit)
        ]
    );
}

async fn reservation_date(tx: &mut Transaction, mv: i64) -> Option<String> {
    tx.query(
        "SELECT reservation_date::text FROM stock_move WHERE id = $1",
        &[&mv],
    )
    .await
    .unwrap()[0]
        .get(0)
}

#[tokio::test]
async fn confirming_a_delivery_reserves_at_confirmation() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "4")
        .await;
    assert_eq!(picking_state(&mut tx, picking).await, "draft");
    f.call(&mut tx, picking, "action_confirm").await.unwrap();
    let today = f.env(&tx).context_today().unwrap().to_string();
    assert_eq!(reservation_date(&mut tx, mv).await, Some(today));
    assert_eq!(move_row(&mut tx, mv).await.2, "assigned");
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("4"))));
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
    // the overview card counts the transfer as ready
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let env = f.env(&tx);
    let types = env
        .records("stock.picking.type")
        .unwrap()
        .browse([f.delivery_type])
        .unwrap();
    assert_eq!(
        types.cell(&mut tx, "count_picking_ready").await.unwrap(),
        Cell::Int(1)
    );
}

/// availability does)
#[tokio::test]
async fn manual_types_wait_for_check_availability() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    let env = f.env(&tx);
    env.records("stock.picking.type")
        .unwrap()
        .browse([f.delivery_type])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("reservation_method", text("manual")),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "4")
        .await;
    f.call(&mut tx, picking, "action_confirm").await.unwrap();
    assert_eq!(reservation_date(&mut tx, mv).await, None);
    assert_eq!(move_row(&mut tx, mv).await.2, "confirmed");
    assert_eq!(picking_state(&mut tx, picking).await, "confirmed");
    assert_eq!(
        f.quant(&mut tx, f.p).await,
        Some((dec("10"), Decimal::ZERO))
    );
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(move_row(&mut tx, mv).await.2, "assigned");
    f.call(&mut tx, picking, "do_unreserve").await.unwrap();
    assert_eq!(move_row(&mut tx, mv).await.2, "confirmed");
    assert_eq!(
        f.quant(&mut tx, f.p).await,
        Some((dec("10"), Decimal::ZERO))
    );
}

#[tokio::test]
async fn a_confirmed_receipt_is_ready_at_once() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.receipt_type, f.vendor, f.l).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.vendor, f.l, "5")
        .await;
    f.call(&mut tx, picking, "action_confirm").await.unwrap();
    assert_eq!(move_row(&mut tx, mv).await.2, "assigned");
    assert_eq!(lines_of(&mut tx, mv).await, vec![(dec("5"), f.vendor)]);
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
}

#[tokio::test]
async fn confirmation_waits_behind_origins_and_refuses_orders() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let origin = f.move_in(&mut tx, None, f.p, f.vendor, f.l, "5").await;
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "5")
        .await;
    let env = f.env(&tx);
    env.records("stock.move")
        .unwrap()
        .browse([mv])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_commands(
                "move_orig_ids",
                vec![Command::Link(RecId::from_real(origin).unwrap())],
            ),
        )
        .await
        .unwrap();
    f.call(&mut tx, picking, "action_confirm").await.unwrap();
    assert_eq!(move_row(&mut tx, mv).await.2, "waiting");
    assert_eq!(picking_state(&mut tx, picking).await, "waiting");
    // a transfer whose moves are all confirmed has nothing left to confirm
    let error = f
        .call(&mut tx, picking, "action_confirm")
        .await
        .unwrap_err();
    user_error(&error, "nothing to confirm");

    let other = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let ordered = f
        .move_in(&mut tx, Some(other), f.p, f.l, f.customer, "1")
        .await;
    env.records("stock.move")
        .unwrap()
        .browse([ordered])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("procure_method", text("make_to_order")),
        )
        .await
        .unwrap();
    let error = f.call(&mut tx, other, "action_confirm").await.unwrap_err();
    user_error(&error, "a later release");
    assert_eq!(move_row(&mut tx, ordered).await.2, "draft");
}

/// Check availability confirms a draft transfer first.
#[tokio::test]
async fn check_availability_confirms_a_draft_transfer() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "2").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "3")
        .await;
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(move_row(&mut tx, mv).await.2, "partially_available");
    assert_eq!(lines_of(&mut tx, mv).await, vec![(dec("2"), f.l)]);
}

impl Fixture {
    async fn validate(&self, tx: &mut Transaction, picking: i64) -> Result<()> {
        self.call(tx, picking, "button_validate").await
    }

    /// Confirm `picking` and check its availability, as a person does (no state is written).
    async fn confirm_and_assign(&self, tx: &mut Transaction, picking: i64) {
        self.call(tx, picking, "action_confirm").await.unwrap();
        self.call(tx, picking, "action_assign").await.unwrap();
    }

    /// A receipt from V to L with one move of `qty` of P, confirmed and ready (the vendor bypasses
    /// reservation).
    async fn ready_receipt(&self, tx: &mut Transaction, qty: &str) -> (i64, i64) {
        let picking = self
            .picking(tx, self.receipt_type, self.vendor, self.l)
            .await;
        let mv = self
            .move_in(tx, Some(picking), self.p, self.vendor, self.l, qty)
            .await;
        self.confirm_and_assign(tx, picking).await;
        (picking, mv)
    }

    /// A draft delivery from L to X with one move of `qty` of `product`.
    async fn draft_delivery(&self, tx: &mut Transaction, product: i64, qty: &str) -> (i64, i64) {
        let picking = self
            .picking(tx, self.delivery_type, self.l, self.customer)
            .await;
        let mv = self
            .move_in(tx, Some(picking), product, self.l, self.customer, qty)
            .await;
        (picking, mv)
    }

    /// A delivery of `qty` of `product`, confirmed and reserved from what is free at L.
    async fn ready_delivery(&self, tx: &mut Transaction, product: i64, qty: &str) -> (i64, i64) {
        let (picking, mv) = self.draft_delivery(tx, product, qty).await;
        self.confirm_and_assign(tx, picking).await;
        (picking, mv)
    }

    /// A savepointed write of `model` records `ids`, as the caller or with sudo.
    async fn write_as(
        &self,
        tx: &mut Transaction,
        model: &str,
        ids: &[i64],
        vals: RawVals,
        sudo: bool,
    ) -> Result<()> {
        let env = self.env(tx).sudo(sudo);
        tx.savepoint(async |tx| {
            env.records(model)?
                .browse(ids.iter().copied())?
                .write(tx, vals)
                .await
        })
        .await
    }

    async fn unlink_as(&self, tx: &mut Transaction, model: &str, ids: &[i64]) -> Result<()> {
        let env = self.env(tx);
        tx.savepoint(async |tx| {
            env.records(model)?
                .browse(ids.iter().copied())?
                .unlink(tx)
                .await
        })
        .await
    }
}

/// `(state, picked, done time set)` of a move, from the table.
async fn move_done(tx: &mut Transaction, id: i64) -> (String, bool, bool) {
    let row = tx
        .query(
            "SELECT state, picked, done_at IS NOT NULL FROM stock_move WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .remove(0);
    (row.get(0), row.get(1), row.get(2))
}

/// The memberships of a move's lines, by id.
async fn memberships(tx: &mut Transaction, mv: i64) -> Vec<String> {
    tx.query(
        "SELECT membership FROM stock_move_line WHERE move_id = $1 ORDER BY id",
        &[&mv],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| row.get(0))
    .collect()
}

async fn ready_count(tx: &mut Transaction, picking_type: i64) -> Cell {
    tx.env()
        .records("stock.picking.type")
        .unwrap()
        .browse([picking_type])
        .unwrap()
        .cell(tx, "count_picking_ready")
        .await
        .unwrap()
}

#[tokio::test]
async fn validating_a_receipt_puts_its_stock_on_hand() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.ready_receipt(&mut tx, "7").await;
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
    assert_eq!(ready_count(&mut tx, f.receipt_type).await, Cell::Int(1));
    f.validate(&mut tx, picking).await.unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("7"), Decimal::ZERO)));
    assert_eq!(
        move_done(&mut tx, mv).await,
        ("done".to_owned(), true, true)
    );
    assert_eq!(move_row(&mut tx, mv).await.0, dec("7"));
    assert_eq!(memberships(&mut tx, mv).await, ["done"]);
    assert_eq!(picking_state(&mut tx, picking).await, "done");
    let done_date: bool = tx
        .query(
            "SELECT date_done IS NOT NULL FROM stock_picking WHERE id = $1",
            &[&picking],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert!(done_date);
    // the overview counter follows in the same transaction
    assert_eq!(ready_count(&mut tx, f.receipt_type).await, Cell::Int(0));
    tx.commit().await.unwrap();
    // a committed reader sees the same
    let mut tx = f.h.begin().await.unwrap();
    assert_eq!(picking_state(&mut tx, picking).await, "done");
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("7"), Decimal::ZERO)));
}

/// engine's underflow refusal, tested with the engine)
#[tokio::test]
async fn validating_a_delivery_releases_its_reservation_and_takes_the_stock() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.draft_delivery(&mut tx, f.p, "5").await;
    let consumable = f
        .move_in(&mut tx, Some(picking), f.consumable, f.l, f.customer, "2")
        .await;
    f.confirm_and_assign(&mut tx, picking).await;
    // an extra zero line of the delivery is deleted on validation
    let zero = f
        .add_lines(
            &mut tx,
            vec![line(mv, "0").set_cell("location_dest_id", Cell::Id(f.vendor))],
        )
        .await
        .unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("5"))));
    f.validate(&mut tx, picking).await.unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), Decimal::ZERO)));
    assert_eq!(move_done(&mut tx, mv).await.0, "done");
    assert_eq!(move_done(&mut tx, consumable).await.0, "done");
    assert_eq!(f.quant_rows(&mut tx, f.consumable).await, 0);
    let left: i64 = tx
        .query(
            "SELECT count(*) FROM stock_move_line WHERE id = $1",
            &[&zero[0]],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(left, 0);
    assert_eq!(memberships(&mut tx, mv).await, ["done"]);
    assert_eq!(picking_state(&mut tx, picking).await, "done");
}

/// reference also cancels the unpicked remainder moves)
#[tokio::test]
async fn validation_refusals_are_named() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let empty = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let error = f.validate(&mut tx, empty).await.unwrap_err();
    user_error(&error, "without products has nothing to validate");
    // a cancelled transfer is not validated
    let (cancelled, _) = f.draft_delivery(&mut tx, f.p, "1").await;
    f.call(&mut tx, cancelled, "action_cancel").await.unwrap();
    let error = f.validate(&mut tx, cancelled).await.unwrap_err();
    user_error(&error, "cancelled transfer");
    // nothing reserved: no quantity to validate
    let (picking, _) = f.ready_delivery(&mut tx, f.p, "10").await;
    let error = f.validate(&mut tx, picking).await.unwrap_err();
    user_error(&error, "no quantity to validate");
    tx.rollback().await.unwrap();

    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "4").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.ready_delivery(&mut tx, f.p, "10").await;
    // 4 of 10: the rest would need a backorder
    let error = f.validate(&mut tx, picking).await.unwrap_err();
    user_error(&error, "short of its demand");
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("4"), dec("4"))));
    assert_eq!(move_done(&mut tx, mv).await.0, "partially_available");
    // an operation type that never creates backorders validates what is there
    f.write_as(
        &mut tx,
        "stock.picking.type",
        &[f.delivery_type],
        RawVals::new().set_cell("create_backorder", text("never")),
        false,
    )
    .await
    .unwrap();
    f.validate(&mut tx, picking).await.unwrap();
    assert_eq!(f.quant_rows(&mut tx, f.p).await, 0);
    assert_eq!(move_done(&mut tx, mv).await.0, "done");
    assert_eq!(move_row(&mut tx, mv).await.0, dec("4"));
    let error = f.validate(&mut tx, picking).await.unwrap_err();
    user_error(&error, "validated earlier");
}

/// A validation that would take a location that forbids negative stock below zero is refused by
/// name and changes nothing (7).
#[tokio::test]
async fn a_validation_below_zero_at_a_flagged_location_changes_nothing() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "5").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.ready_delivery(&mut tx, f.p, "8").await;
    // unflagged, a line may reserve more than is free, as in the reference
    let lines: Vec<i64> = tx
        .query("SELECT id FROM stock_move_line WHERE move_id = $1", &[&mv])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    f.write_lines(
        &mut tx,
        &lines,
        RawVals::new().set_cell("quantity", Cell::Decimal(dec("8"))),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    tx.execute(
        "UPDATE stock_location_policy SET forbid_negative = true WHERE location_id = $1",
        &[&f.l],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let error = f.validate(&mut tx, picking).await.unwrap_err();
    assert!(
        error.to_string().contains("does not allow negative stock"),
        "{error}"
    );
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), dec("8"))));
    assert_eq!(move_done(&mut tx, mv).await.0, "assigned");
    assert_eq!(memberships(&mut tx, mv).await, ["open"]);
    assert_eq!(picking_state(&mut tx, picking).await, "assigned");
}

/// quantity at a flagged location and take reserved stock elsewhere)
#[tokio::test]
async fn a_draft_validation_takes_no_stock_reserved_for_another_transfer_at_a_flagged_location() {
    let f = fixture().await;
    for flagged in [true, false] {
        let mut tx = f.h.begin().await.unwrap();
        tx.execute(
            "UPDATE stock_location_policy SET forbid_negative = $2 WHERE location_id = $1",
            &[&f.l, &flagged],
        )
        .await
        .unwrap();
        f.stock(&mut tx, f.p, "5").await;
        let (first, first_move) = f.ready_delivery(&mut tx, f.p, "5").await;
        assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), dec("5"))));
        let (second, second_move) = f.draft_delivery(&mut tx, f.p, "5").await;
        let validated = f.validate(&mut tx, second).await;
        if flagged {
            // nothing is free: refused by name, and nothing of either transfer changes
            let error = validated.unwrap_err();
            assert!(
                error.to_string().contains("does not allow negative stock")
                    && error.to_string().contains("is free to reserve"),
                "{error}"
            );
            assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), dec("5"))));
            assert_eq!(picking_state(&mut tx, second).await, "draft");
            assert!(lines_of(&mut tx, second_move).await.is_empty());
            // with free stock the same validation goes through
            f.stock(&mut tx, f.p, "5").await;
            f.validate(&mut tx, second).await.unwrap();
            assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), dec("5"))));
        } else {
            // over-reservation is legal where negative stock is allowed, as in the reference
            validated.unwrap();
            assert_eq!(f.quant(&mut tx, f.p).await, Some((Decimal::ZERO, dec("5"))));
        }
        assert_eq!(picking_state(&mut tx, second).await, "done");
        assert_eq!(picking_state(&mut tx, first).await, "assigned");
        assert_eq!(lines_of(&mut tx, first_move).await, vec![(dec("5"), f.l)]);
        tx.rollback().await.unwrap();
    }
}

/// Two sessions validate the same delivery: one moves the stock, the other gets a retryable
/// serialization failure, and its retry is refused because the transfer is done; the stock moves
/// once.
#[tokio::test]
async fn two_sessions_validating_one_transfer_move_the_stock_once() {
    let f = fixture().await;
    // shortcut: setup shares one transaction because a fifth transaction here timed out waiting
    // for a pool connection (pool of 4, cause not found); split it again once that is fixed
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    let (picking, _) = f.ready_delivery(&mut tx, f.p, "5").await;
    tx.commit().await.unwrap();

    // the late session's snapshot is taken before the first one validates
    let mut late = f.h.begin().await.unwrap();
    assert_eq!(picking_state(&mut late, picking).await, "assigned");
    let mut tx = f.h.begin().await.unwrap();
    f.validate(&mut tx, picking).await.unwrap();
    tx.commit().await.unwrap();
    let error = f.validate(&mut late, picking).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Concurrency, "{error:?}");
    assert_eq!(error.sqlstate(), Some("40001"), "{error:?}");
    late.rollback().await.unwrap();

    let mut tx = f.h.begin().await.unwrap();
    let error = f.validate(&mut tx, picking).await.unwrap_err();
    user_error(&error, "validated earlier");
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("5"), Decimal::ZERO)));
}

#[tokio::test]
async fn done_work_is_fixed_for_every_caller() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.ready_receipt(&mut tx, "3").await;
    f.validate(&mut tx, picking).await.unwrap();
    let lines: Vec<i64> = tx
        .query("SELECT id FROM stock_move_line WHERE move_id = $1", &[&mv])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();

    // a done move: demand, unit and locations are refused, also with sudo (its state and
    // quantity are refused for every move)
    for sudo in [false, true] {
        for vals in [
            RawVals::new().set_cell("uom_id", Cell::Id(f.dozen)),
            RawVals::new().set_cell("product_uom_qty", Cell::Decimal(dec("9"))),
            RawVals::new().set_cell("location_dest_id", Cell::Id(f.customer)),
        ] {
            let error = f
                .write_as(&mut tx, "stock.move", &[mv], vals, sudo)
                .await
                .unwrap_err();
            user_error(&error, "of a done stock move cannot be changed");
        }
    }
    // what the allow-list keeps writable
    f.write_as(
        &mut tx,
        "stock.move",
        &[mv],
        RawVals::new().set_cell("origin", text("PO7")),
        false,
    )
    .await
    .unwrap();
    // a done transfer: its locations and its moves (also as x2many commands) are refused
    let error = f
        .write_as(
            &mut tx,
            "stock.picking",
            &[picking],
            RawVals::new().set_cell("location_id", Cell::Id(f.customer)),
            true,
        )
        .await
        .unwrap_err();
    user_error(&error, "of a done transfer cannot be changed");
    let error = f
        .write_as(
            &mut tx,
            "stock.picking",
            &[picking],
            RawVals::new().set_commands(
                "move_ids",
                vec![Command::Update(
                    RecId::from_real(mv).unwrap(),
                    RawVals::new().set_cell("product_uom_qty", Cell::Decimal(dec("9"))),
                )],
            ),
            false,
        )
        .await
        .unwrap_err();
    user_error(&error, "of a done transfer cannot be changed");
    f.write_as(
        &mut tx,
        "stock.picking",
        &[picking],
        RawVals::new().set_cell("note", text("checked")),
        false,
    )
    .await
    .unwrap();
    // no move joins a done transfer, in the values or as the context default
    let error = tx
        .savepoint(async |tx| {
            f.env(tx)
                .records("stock.move")?
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("picking_id", Cell::Id(picking))
                            .set_cell("product_id", Cell::Id(f.p))
                            .set_cell("product_uom_qty", Cell::Decimal(dec("1"))),
                    ],
                )
                .await
        })
        .await
        .unwrap_err();
    user_error(&error, "of a done transfer cannot be changed");
    let error = tx
        .savepoint(async |tx| {
            f.env(tx)
                .with_context([("default_picking_id", Value::Int(picking))])
                .records("stock.move")?
                .create(
                    tx,
                    vec![
                        RawVals::new()
                            .set_cell("product_id", Cell::Id(f.p))
                            .set_cell("product_uom_qty", Cell::Decimal(dec("1"))),
                    ],
                )
                .await
        })
        .await
        .unwrap_err();
    user_error(&error, "of a done transfer cannot be changed");
    // done lines, moves and transfers are not deleted; done lines are not changed or added to
    let error = f
        .write_lines(
            &mut tx,
            &lines,
            RawVals::new().set_cell("quantity", Cell::Decimal(dec("1"))),
        )
        .await
        .unwrap_err();
    user_error(&error, "done operation line");
    let error = f.unlink_lines(&mut tx, &lines).await.unwrap_err();
    user_error(&error, "done operation line");
    let error = f.add_lines(&mut tx, vec![line(mv, "1")]).await.unwrap_err();
    user_error(&error, "done or cancelled move");
    let error = f.unlink_as(&mut tx, "stock.move", &[mv]).await.unwrap_err();
    user_error(&error, "A done stock move cannot be deleted");
    let error = f
        .unlink_as(&mut tx, "stock.picking", &[picking])
        .await
        .unwrap_err();
    user_error(&error, "A done transfer cannot be deleted");
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("3"), Decimal::ZERO)));
}

#[tokio::test]
async fn a_row_added_in_the_save_that_changes_the_header_takes_the_new_header() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let partner = create(
        &mut tx,
        "res.partner",
        RawVals::new().set_cell("name", text("Customer B")),
    )
    .await;
    let other = location(&mut tx, "L2", "internal").await;
    let picking = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    // one save of the transfer form: a new source and contact, and a new operation row
    f.env(&tx)
        .records("stock.picking")
        .unwrap()
        .browse([picking])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("location_id", Cell::Id(other))
                .set_cell("partner_id", Cell::Id(partner))
                .set_commands(
                    "move_ids",
                    vec![Command::Create(
                        RawVals::new()
                            .set_cell("product_id", Cell::Id(f.p))
                            .set_cell("product_uom_qty", Cell::Decimal(dec("2"))),
                    )],
                ),
        )
        .await
        .unwrap();
    let mv: i64 = tx
        .query(
            "SELECT id FROM stock_move WHERE picking_id = $1",
            &[&picking],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(
        move_fields(
            &mut tx,
            mv,
            &["location_id", "location_dest_id", "partner_id"]
        )
        .await,
        vec![Some(other), Some(f.customer), Some(partner)]
    );
}

/// A move's status and quantity follow from its lines, confirmation and reservation: no caller
/// names them, not in the values, not through the transfer's rows and not as a context default.
#[tokio::test]
async fn move_status_and_quantity_are_refused_for_every_caller() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.manual_type, f.l, f.customer).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.l, f.customer, "5")
        .await;
    let label = |tx: &Transaction, field: &str| {
        tx.registry()
            .model("stock.move")
            .unwrap()
            .field(field)
            .unwrap()
            .string
            .to_string()
    };
    let expect = |error: Error, field: String, default: bool| {
        assert_eq!(error.kind(), ErrorKind::Programming, "{error:?}");
        assert_eq!(
            error.message(),
            erp_addon_stock::StockError::DerivedFromLines { field, default }.to_string()
        );
    };
    for (field, value) in [
        ("state", text("assigned")),
        ("quantity", Cell::Decimal(dec("5"))),
    ] {
        for sudo in [false, true] {
            let env = f.env(&tx).sudo(sudo);
            let moves = env.records("stock.move").unwrap().browse([mv]).unwrap();
            let vals = RawVals::new().set_cell(field, value.clone());
            let error = tx
                .savepoint(async |tx| moves.write(tx, vals).await)
                .await
                .unwrap_err();
            expect(error, label(&tx, field), false);
            let pickings = env
                .records("stock.picking")
                .unwrap()
                .browse([picking])
                .unwrap();
            let vals = RawVals::new().set_commands(
                "move_ids",
                vec![Command::Update(
                    RecId::from_real(mv).unwrap(),
                    RawVals::new().set_cell(field, value.clone()),
                )],
            );
            let error = tx
                .savepoint(async |tx| pickings.write(tx, vals).await)
                .await
                .unwrap_err();
            expect(error, label(&tx, field), false);
            let vals = RawVals::new()
                .set_cell("picking_id", Cell::Id(picking))
                .set_cell("product_id", Cell::Id(f.p))
                .set_cell(field, value.clone());
            let model = env.records("stock.move").unwrap();
            let error = tx
                .savepoint(async |tx| model.create(tx, vec![vals]).await)
                .await
                .unwrap_err();
            expect(error, label(&tx, field), false);
            let model = env
                .with_context([(format!("default_{field}"), Value::str("1"))])
                .records("stock.move")
                .unwrap();
            let vals = RawVals::new()
                .set_cell("picking_id", Cell::Id(picking))
                .set_cell("product_id", Cell::Id(f.p));
            let error = tx
                .savepoint(async |tx| model.create(tx, vec![vals]).await)
                .await
                .unwrap_err();
            expect(error, label(&tx, field), true);
        }
    }
    // the product-unit quantity follows the quantity: another value is refused by the snapshot
    let moves = f
        .env(&tx)
        .records("stock.move")
        .unwrap()
        .browse([mv])
        .unwrap();
    let error = tx
        .savepoint(async |tx| {
            moves
                .write(
                    tx,
                    RawVals::new().set_cell("quantity_product_uom", Cell::Decimal(dec("5"))),
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Programming, "{error:?}");
    assert_eq!(
        error.message(),
        erp_addon_stock::StockError::UnitSnapshot {
            field: label(&tx, "quantity_product_uom")
        }
        .to_string()
    );
    // a copy starts draft with no quantity, whatever the original holds
    f.stock(&mut tx, f.p, "5").await;
    f.confirm(&mut tx, picking).await;
    f.call(&mut tx, picking, "action_assign").await.unwrap();
    assert_eq!(move_row(&mut tx, mv).await.2, "assigned");
    let copy = moves.copy(&mut tx, None).await.unwrap().ids()[0];
    let (quantity, product_quantity, state, _) = move_row(&mut tx, copy).await;
    assert_eq!(
        (quantity, product_quantity, state.as_str()),
        (Decimal::ZERO, Decimal::ZERO, "draft")
    );
    let (quantity, _, state, _) = move_row(&mut tx, mv).await;
    assert_eq!((quantity, state.as_str()), (dec("5"), "assigned"));
}

async fn picking_row(tx: &mut Transaction, id: i64) -> (String, i64, i64, i64) {
    let row = tx
        .query(
            "SELECT name, location_id, location_dest_id, company_id FROM stock_picking \
             WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .remove(0);
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

#[tokio::test]
async fn a_new_transfer_takes_its_reference_and_locations_from_its_type() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let numbered = create(
        &mut tx,
        "stock.picking.type",
        RawVals::new()
            .set_cell("name", text("Numbered receipts"))
            .set_cell("code", text("incoming"))
            .set_cell("sequence_code", text("NUM/IN/"))
            .set_cell("company_id", Cell::Id(f.company))
            .set_cell("default_location_src_id", Cell::Id(f.vendor))
            .set_cell("default_location_dest_id", Cell::Id(f.l)),
    )
    .await;
    let env = f.env(&tx);
    let mut names = Vec::new();
    for vals in [
        RawVals::new().set_cell("picking_type_id", Cell::Id(numbered)),
        // what the form sends: the default reference
        RawVals::new()
            .set_cell("picking_type_id", Cell::Id(numbered))
            .set_cell("name", text("/")),
    ] {
        let id = env
            .records("stock.picking")
            .unwrap()
            .create(&mut tx, vec![vals])
            .await
            .unwrap()
            .ids()[0];
        let (name, source, dest, company) = picking_row(&mut tx, id).await;
        assert!(name.starts_with("NUM/IN/"), "{name}");
        assert_eq!((source, dest, company), (f.vendor, f.l, f.company));
        names.push(name);
    }
    assert_ne!(names[0], names[1]);
    // a given reference is kept, and a type without a sequence leaves it to the caller
    let kept = f.picking(&mut tx, f.receipt_type, f.vendor, f.l).await;
    assert!(picking_row(&mut tx, kept).await.0.starts_with('T'));
}

/// the backorder decision is refused by name)
#[tokio::test]
async fn validating_a_draft_confirms_it_and_fills_empty_moves_with_their_demand() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let picking = f.picking(&mut tx, f.receipt_type, f.vendor, f.l).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.vendor, f.l, "4")
        .await;
    assert_eq!(picking_state(&mut tx, picking).await, "draft");
    f.validate(&mut tx, picking).await.unwrap();
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("4"), Decimal::ZERO)));
    assert_eq!(move_done(&mut tx, mv).await.0, "done");
    assert_eq!(move_row(&mut tx, mv).await.0, dec("4"));
    assert_eq!(picking_state(&mut tx, picking).await, "done");

    // an entered quantity is not replaced by the demand
    f.write_as(
        &mut tx,
        "stock.picking.type",
        &[f.receipt_type],
        RawVals::new().set_cell("create_backorder", text("never")),
        false,
    )
    .await
    .unwrap();
    let picking = f.picking(&mut tx, f.receipt_type, f.vendor, f.l).await;
    let mv = f
        .move_in(&mut tx, Some(picking), f.p, f.vendor, f.l, "4")
        .await;
    f.add_lines(&mut tx, vec![line(mv, "2")]).await.unwrap();
    f.validate(&mut tx, picking).await.unwrap();
    assert_eq!(move_row(&mut tx, mv).await.0, dec("2"));
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("6"), Decimal::ZERO)));
}

/// cleared, the supply method becomes make-to-stock; the origin links stay)
#[tokio::test]
async fn cancel_gives_the_reservation_back_and_cancels_the_transfer() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.ready_delivery(&mut tx, f.p, "6").await;
    assert_eq!(f.quant(&mut tx, f.p).await, Some((dec("10"), dec("6"))));
    f.call(&mut tx, picking, "action_cancel").await.unwrap();
    assert_eq!(
        f.quant(&mut tx, f.p).await,
        Some((dec("10"), Decimal::ZERO))
    );
    assert_eq!(
        move_done(&mut tx, mv).await,
        ("cancel".to_owned(), false, false)
    );
    assert_eq!(move_row(&mut tx, mv).await.0, Decimal::ZERO);
    assert!(lines_of(&mut tx, mv).await.is_empty());
    assert_eq!(picking_state(&mut tx, picking).await, "cancel");
    let locked: bool = tx
        .query(
            "SELECT is_locked FROM stock_picking WHERE id = $1",
            &[&picking],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert!(locked);
    // a done transfer and an empty one are refused by name
    let (done, _) = f.ready_receipt(&mut tx, "1").await;
    f.validate(&mut tx, done).await.unwrap();
    let error = f.call(&mut tx, done, "action_cancel").await.unwrap_err();
    user_error(&error, "A done transfer cannot be cancelled");
    let empty = f.picking(&mut tx, f.delivery_type, f.l, f.customer).await;
    let error = f.call(&mut tx, empty, "action_cancel").await.unwrap_err();
    user_error(&error, "delete it instead");
}

#[tokio::test]
async fn deleting_a_transfer_cancels_and_deletes_its_moves() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "10").await;
    tx.commit().await.unwrap();
    let mut tx = f.h.begin().await.unwrap();
    let (picking, mv) = f.ready_delivery(&mut tx, f.p, "6").await;
    let (draft, draft_move) = f.draft_delivery(&mut tx, f.p, "1").await;
    f.unlink_as(&mut tx, "stock.picking", &[picking, draft])
        .await
        .unwrap();
    assert_eq!(
        f.quant(&mut tx, f.p).await,
        Some((dec("10"), Decimal::ZERO))
    );
    let left: i64 = tx
        .query(
            "SELECT count(*) FROM stock_move WHERE id = ANY($1)",
            &[&vec![mv, draft_move]],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(left, 0);
    let left: i64 = tx
        .query(
            "SELECT count(*) FROM stock_picking WHERE id = ANY($1)",
            &[&vec![picking, draft]],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(left, 0);
}
