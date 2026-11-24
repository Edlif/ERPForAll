// SPDX-License-Identifier: MIT OR Apache-2.0
//! Batch transfers: adding transfers, the batch states and the chunked validation with a result per
//! transfer. Every compared value is read back from the database.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "tests"
)]

use erp_addon_stock::{QuantChange, QuantKey, apply_quant_changes};
use erp_sdk::ErrorKind;
use erp_sdk::jobs::{StepArgs, StepOutcome};
use erp_sdk::prelude::*;
use erp_testkit::Harness;

erp_sdk::addon!(name = "test_stock_batch", depends = ["stock"]);

/// A test stand-in for `ir.model.data` (the real model lives in `erp-data`); the same as the other
/// stock tests.
#[model(
    name = "ir.model.data",
    description = "Model Data",
    addon = "test_stock_batch"
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
    let builder = Harness::builder([
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
    .schema_sync_options(erp_testkit::SchemaSyncOptions::inline().lifecycle(true));
    let h = builder.build().await.unwrap();
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
}

fn ids(list: &[i64]) -> Cell {
    Cell::Ids(
        list.iter()
            .map(|id| RecId::from_real(*id).unwrap())
            .collect(),
    )
}

impl Fixture {
    async fn call(&self, tx: &mut Transaction, picking: i64, method: &'static str) -> Result<()> {
        let records = self.env(tx).records("stock.picking")?.browse([picking])?;
        tx.savepoint(async move |tx| records.call::<(), ()>(tx, method, ()).await)
            .await
    }

    /// A transfer of `picking_type` (receipt or delivery) with one move of `qty` of P, confirmed
    /// (and reserved at confirmation for a delivery).
    async fn transfer(&self, tx: &mut Transaction, receipt: bool, qty: &str) -> i64 {
        let (kind, src, dest) = if receipt {
            (self.receipt_type, self.vendor, self.l)
        } else {
            (self.delivery_type, self.l, self.customer)
        };
        let picking = self.picking(tx, kind, src, dest).await;
        self.move_in(tx, Some(picking), self.p, src, dest, qty)
            .await;
        self.confirm(tx, picking).await;
        picking
    }

    /// A draft receipt: its move is not confirmed.
    async fn draft_receipt(&self, tx: &mut Transaction) -> i64 {
        let picking = self
            .picking(tx, self.receipt_type, self.vendor, self.l)
            .await;
        self.move_in(tx, Some(picking), self.p, self.vendor, self.l, "1")
            .await;
        picking
    }

    /// A batch of this company, named from a sequence made here (the data file's own record is not
    /// loaded by this harness).
    async fn batch(&self, tx: &mut Transaction) -> Result<i64> {
        let existing = tx
            .query(
                "SELECT 1 FROM ir_sequence WHERE code = 'stock.picking.batch'",
                &[],
            )
            .await
            .unwrap();
        if existing.is_empty() {
            create(
                tx,
                "ir.sequence",
                RawVals::new()
                    .set_cell("name", text("Batch transfer"))
                    .set_cell("code", text("stock.picking.batch"))
                    .set_cell("prefix", text("BATCH/"))
                    .set_cell("padding", Cell::Int(5)),
            )
            .await;
        }
        let env = self.env(tx);
        Ok(env
            .records("stock.picking.batch")?
            .create(
                tx,
                vec![RawVals::new().set_cell("description", text("Morning wave"))],
            )
            .await?
            .ids()[0])
    }

    fn batches(&self, tx: &Transaction, batch: i64) -> Records {
        self.env(tx)
            .records("stock.picking.batch")
            .unwrap()
            .browse([batch])
            .unwrap()
    }

    async fn call_batch(
        &self,
        tx: &mut Transaction,
        batch: i64,
        method: &'static str,
    ) -> Result<()> {
        let records = self.batches(tx, batch);
        tx.savepoint(async move |tx| records.call::<(), ()>(tx, method, ()).await)
            .await
    }

    /// Add `list` through the dialog model, as the Add to Batch button does.
    async fn add(&self, tx: &mut Transaction, batch: i64, list: &[i64]) -> Result<()> {
        let env = self.env(tx);
        let wizard = env
            .records("stock.picking.to.batch")?
            .create(
                tx,
                vec![
                    RawVals::new()
                        .set_cell("mode", text("existing"))
                        .set_cell("batch_id", Cell::Id(batch))
                        .set_cell("picking_ids", ids(list)),
                ],
            )
            .await?;
        tx.savepoint(async move |tx| {
            wizard
                .call::<(), Value>(tx, "action_add_to_batch", ())
                .await
                .map(drop)
        })
        .await
    }

    /// The payload the queued job of `run` stores, with its kind and key checked.
    async fn queued(
        &self,
        tx: &mut Transaction,
        batch: i64,
        run: i32,
    ) -> erp_sdk::serde_json::Value {
        tx.flush().await.unwrap();
        let key = format!("stock.batch_validate:{batch}:{run}");
        let rows = tx
            .query(
                "SELECT kind, payload::text FROM erp_job WHERE dedup_key = $1 AND state <> 'dead'",
                &[&key],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "one live job for {key}");
        assert_eq!(rows[0].get::<_, String>(0), "stock.batch_validate");
        erp_sdk::serde_json::from_str(&rows[0].get::<_, String>(1)).unwrap()
    }

    /// One chunk of the validation job of `run`, fed the payload the queue stored.
    async fn step(&self, tx: &mut Transaction, batch: i64, run: i32) -> StepOutcome {
        let args = StepArgs {
            payload: self.queued(tx, batch, run).await,
            progress: None,
            attempt: 1,
        };
        self.env(tx)
            .records("stock.picking.batch")
            .unwrap()
            .call::<(StepArgs,), StepOutcome>(tx, "run_validate_step", (args,))
            .await
            .unwrap()
    }
}

async fn picking_state(tx: &mut Transaction, id: i64) -> String {
    tx.flush().await.unwrap();
    tx.query("SELECT state FROM stock_picking WHERE id = $1", &[&id])
        .await
        .unwrap()[0]
        .get(0)
}

async fn batch_row(tx: &mut Transaction, id: i64) -> (String, bool, i32, Option<i64>) {
    tx.flush().await.unwrap();
    let row = tx
        .query(
            "SELECT state, validating, validation_run, picking_type_id FROM stock_picking_batch WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .remove(0);
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

async fn picking_batch(tx: &mut Transaction, id: i64) -> Option<i64> {
    tx.flush().await.unwrap();
    tx.query("SELECT batch_id FROM stock_picking WHERE id = $1", &[&id])
        .await
        .unwrap()[0]
        .get(0)
}

/// `(picking, outcome, message)` of the results of a run, by transfer.
async fn results(tx: &mut Transaction, batch: i64, run: i32) -> Vec<(i64, String, Option<String>)> {
    tx.flush().await.unwrap();
    tx.query(
        "SELECT picking_id, outcome, message FROM stock_picking_batch_result \
         WHERE batch_id = $1 AND run = $2 ORDER BY picking_id",
        &[&batch, &run],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1), row.get(2)))
    .collect()
}

/// A new batch is named from the sequence, takes the company and the operation type of the first
/// transfer, and refuses a transfer that does not fit, naming it and adding none.
#[tokio::test]
async fn transfers_join_a_batch_only_when_they_fit() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "5").await;
    let batch = f.batch(&mut tx).await.unwrap();
    let name: String = tx
        .query(
            "SELECT name FROM stock_picking_batch WHERE id = $1",
            &[&batch],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(name, "BATCH/00001");
    assert_eq!(batch_row(&mut tx, batch).await.0, "draft");

    let first = f.transfer(&mut tx, true, "1").await;
    let second = f.transfer(&mut tx, true, "1").await;
    let delivery = f.transfer(&mut tx, false, "1").await;
    f.add(&mut tx, batch, &[first]).await.unwrap();
    assert_eq!(picking_batch(&mut tx, first).await, Some(batch));
    assert_eq!(batch_row(&mut tx, batch).await.3, Some(f.receipt_type));

    // one transfer of another operation type: it is named and the compatible one is not added either
    let error = f
        .add(&mut tx, batch, &[second, delivery])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    let delivery_name: String = tx
        .query("SELECT name FROM stock_picking WHERE id = $1", &[&delivery])
        .await
        .unwrap()[0]
        .get(0);
    assert!(
        error.message().contains(&delivery_name),
        "{}",
        error.message()
    );
    assert_eq!(picking_batch(&mut tx, second).await, None);
    assert_eq!(picking_batch(&mut tx, delivery).await, None);

    // a transfer in another batch does not move silently
    let other = f.batch(&mut tx).await.unwrap();
    let error = f.add(&mut tx, other, &[first]).await.unwrap_err();
    assert!(
        error.message().contains("another batch"),
        "{}",
        error.message()
    );

    // the status and the transfer list are not written directly
    for vals in [
        RawVals::new().set_cell("state", text("done")),
        RawVals::new().set_cell("validating", Cell::Bool(true)),
        RawVals::new().set_cell("picking_ids", ids(&[second])),
    ] {
        let error = f
            .batches(&tx, batch)
            .write(&mut tx, vals)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    }
    assert_eq!(batch_row(&mut tx, batch).await.0, "draft");
}

/// Confirm refuses an empty batch, confirms the draft transfers and starts the batch; a draft
/// transfer cannot join a batch that is already in progress.
#[tokio::test]
async fn confirming_starts_the_batch_and_its_draft_transfers() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let batch = f.batch(&mut tx).await.unwrap();
    let error = f
        .call_batch(&mut tx, batch, "action_confirm")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert_eq!(batch_row(&mut tx, batch).await.0, "draft");

    let draft = f.draft_receipt(&mut tx).await;
    assert_eq!(picking_state(&mut tx, draft).await, "draft");
    f.add(&mut tx, batch, &[draft]).await.unwrap();
    f.call_batch(&mut tx, batch, "action_confirm")
        .await
        .unwrap();
    assert_eq!(batch_row(&mut tx, batch).await.0, "in_progress");
    assert_ne!(picking_state(&mut tx, draft).await, "draft");

    let late = f.draft_receipt(&mut tx).await;
    let error = f.add(&mut tx, batch, &[late]).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
    assert_eq!(picking_batch(&mut tx, late).await, None);
}

/// Validation goes chunk by chunk: each transfer has its own result, a failing transfer stays in
/// the batch with its message, a transfer without reserved quantity leaves it, and a repeated
/// chunk of a finished run does nothing.
#[tokio::test]
async fn validation_keeps_a_result_per_transfer_in_chunks() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    f.stock(&mut tx, f.p, "3").await;
    tx.env()
        .records("ir.config_parameter")
        .unwrap()
        .call::<(String, Option<i64>), i64>(
            &mut tx,
            "set_int",
            ("stock.batch_validate_chunk".to_owned(), Some(2)),
        )
        .await
        .unwrap();
    let ready_a = f.transfer(&mut tx, false, "1").await;
    let ready_b = f.transfer(&mut tx, false, "1").await;
    let short = f.transfer(&mut tx, false, "5").await;
    let empty = f.transfer(&mut tx, false, "1").await;
    let batch = f.batch(&mut tx).await.unwrap();
    f.add(&mut tx, batch, &[ready_a, ready_b, short, empty])
        .await
        .unwrap();
    f.call_batch(&mut tx, batch, "action_confirm")
        .await
        .unwrap();
    f.call_batch(&mut tx, batch, "action_done").await.unwrap();
    assert_eq!(
        batch_row(&mut tx, batch).await,
        ("in_progress".into(), true, 1, Some(f.delivery_type))
    );
    // a second click while it runs is refused
    let error = f
        .call_batch(&mut tx, batch, "action_done")
        .await
        .unwrap_err();
    assert!(
        error.message().contains("already being validated"),
        "{}",
        error.message()
    );

    let first = f.step(&mut tx, batch, 1).await;
    assert!(!first.finished);
    assert_eq!((first.processed, first.remaining), (2, Some(2)));
    assert_eq!(
        results(&mut tx, batch, 1).await,
        vec![
            (ready_a, "done".to_owned(), None),
            (ready_b, "done".to_owned(), None)
        ]
    );
    let second = f.step(&mut tx, batch, 1).await;
    assert!(second.finished);
    assert_eq!(second.processed, 2);
    let rows = results(&mut tx, batch, 1).await;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[2].0, short);
    assert_eq!(rows[2].1, "error");
    assert_eq!(
        rows[2].2.as_deref(),
        Some(
            erp_addon_stock::StockError::BackorderNotAvailable {
                product: "P".into()
            }
            .to_string()
            .as_str()
        ),
        "{rows:?}"
    );
    assert_eq!((rows[3].0, rows[3].1.as_str()), (empty, "detached"));

    assert_eq!(picking_state(&mut tx, ready_a).await, "done");
    assert_eq!(picking_state(&mut tx, ready_b).await, "done");
    assert_ne!(picking_state(&mut tx, short).await, "done");
    assert_eq!(picking_batch(&mut tx, short).await, Some(batch));
    assert_eq!(picking_batch(&mut tx, empty).await, None);
    // the failed transfer keeps the batch open; the validation is over
    assert_eq!(batch_row(&mut tx, batch).await.0, "in_progress");
    assert!(!batch_row(&mut tx, batch).await.1);

    // the finished run is not run again
    let again = f.step(&mut tx, batch, 1).await;
    assert_eq!(again.processed, 0);
    assert_eq!(results(&mut tx, batch, 1).await.len(), 4);
}

/// A batch whose transfers are all validated is done and stays; it is not deleted. A cancelled
/// batch lets its open transfers go.
#[tokio::test]
async fn a_done_batch_stays_and_a_cancelled_batch_releases_its_transfers() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = f.transfer(&mut tx, true, "1").await;
    let b = f.transfer(&mut tx, true, "2").await;
    let batch = f.batch(&mut tx).await.unwrap();
    f.add(&mut tx, batch, &[a, b]).await.unwrap();
    f.call_batch(&mut tx, batch, "action_confirm")
        .await
        .unwrap();
    f.call_batch(&mut tx, batch, "action_done").await.unwrap();
    let outcome = f.step(&mut tx, batch, 1).await;
    assert!(outcome.finished);
    assert_eq!(batch_row(&mut tx, batch).await.0, "done");
    assert!(!batch_row(&mut tx, batch).await.1);
    let error = f.batches(&tx, batch).unlink(&mut tx).await.unwrap_err();
    assert!(
        error.message().contains("done transfer"),
        "{}",
        error.message()
    );
    let error = f
        .call_batch(&mut tx, batch, "action_cancel")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");

    let c = f.transfer(&mut tx, true, "1").await;
    let open = f.batch(&mut tx).await.unwrap();
    f.add(&mut tx, open, &[c]).await.unwrap();
    f.call_batch(&mut tx, open, "action_cancel").await.unwrap();
    assert_eq!(batch_row(&mut tx, open).await.0, "cancel");
    assert_eq!(picking_batch(&mut tx, c).await, None);
    assert_eq!(picking_state(&mut tx, c).await, "assigned");
}

/// The dialog's new-batch mode creates the batch for the transfers' company and confirms it unless
/// it is kept as a draft.
#[tokio::test]
async fn the_dialog_creates_a_new_batch() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = f.transfer(&mut tx, true, "1").await;
    let b = f.transfer(&mut tx, true, "1").await;
    f.batch(&mut tx).await.unwrap();
    let env = f.env(&tx);
    let wizard = env
        .records("stock.picking.to.batch")
        .unwrap()
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("description", text("Dock 3"))
                    .set_cell("is_create_draft", Cell::Bool(false))
                    .set_cell("picking_ids", ids(&[a, b])),
            ],
        )
        .await
        .unwrap();
    wizard
        .call::<(), Value>(&mut tx, "action_add_to_batch", ())
        .await
        .unwrap();
    let batch = picking_batch(&mut tx, a).await.unwrap();
    assert_eq!(picking_batch(&mut tx, b).await, Some(batch));
    let row = batch_row(&mut tx, batch).await;
    assert_eq!(
        (row.0.as_str(), row.3),
        ("in_progress", Some(f.receipt_type))
    );
    let description: String = tx
        .query(
            "SELECT description FROM stock_picking_batch WHERE id = $1",
            &[&batch],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(description, "Dock 3");
}

/// The batch of a transfer, the transfer list and the type and company of a batch are not
/// written around the batch actions.
#[tokio::test]
async fn links_and_batch_identity_are_not_written_directly() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let batch = f.batch(&mut tx).await.unwrap();
    let a = f.transfer(&mut tx, true, "1").await;
    let b = f.transfer(&mut tx, true, "1").await;
    f.add(&mut tx, batch, &[a]).await.unwrap();
    let env = f.env(&tx);
    // a transfer cannot be pointed at a batch, moved or released by a plain write
    for target in [Cell::Id(batch), Cell::Null] {
        let error = env
            .records("stock.picking")
            .unwrap()
            .browse([b])
            .unwrap()
            .write(&mut tx, RawVals::new().set_cell("batch_id", target))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
        assert!(
            error.message().contains("batch actions"),
            "{}",
            error.message()
        );
    }
    let error = env
        .records("stock.picking")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("batch_id", Cell::Id(batch))],
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert_eq!(picking_batch(&mut tx, a).await, Some(batch));
    assert_eq!(picking_batch(&mut tx, b).await, None);
    // a batch is not created with transfers
    let error = env
        .records("stock.picking.batch")
        .unwrap()
        .create(
            &mut tx,
            vec![RawVals::new().set_cell("picking_ids", ids(&[b]))],
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    // while it holds a transfer its type and company stay
    for field in ["picking_type_id", "company_id"] {
        let error = f
            .batches(&tx, batch)
            .write(
                &mut tx,
                RawVals::new().set_cell(field, Cell::Id(f.delivery_type)),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation, "{field} {error:?}");
    }
}

/// A batch that is not done can be deleted; one with a done transfer cannot.
#[tokio::test]
async fn only_a_batch_without_done_work_is_deleted() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = f.transfer(&mut tx, true, "1").await;
    let draft = f.batch(&mut tx).await.unwrap();
    f.batches(&tx, draft).unlink(&mut tx).await.unwrap();
    let batch = f.batch(&mut tx).await.unwrap();
    f.add(&mut tx, batch, &[a]).await.unwrap();
    f.call_batch(&mut tx, batch, "action_confirm")
        .await
        .unwrap();
    f.call_batch(&mut tx, batch, "action_done").await.unwrap();
    // being validated: not deleted either
    let error = f.batches(&tx, batch).unlink(&mut tx).await.unwrap_err();
    assert!(
        error.message().contains("being validated"),
        "{}",
        error.message()
    );
    f.step(&mut tx, batch, 1).await;
    let error = f.batches(&tx, batch).unlink(&mut tx).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UserError, "{error:?}");
}

/// A validation whose job is gone does not hold the batch: Cancel proceeds, and Validate queues
/// the same run again. Nothing joins a batch while a live job validates it.
#[tokio::test]
async fn a_lost_validation_job_does_not_hold_the_batch() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    let a = f.transfer(&mut tx, true, "1").await;
    let b = f.transfer(&mut tx, true, "1").await;
    let batch = f.batch(&mut tx).await.unwrap();
    f.add(&mut tx, batch, &[a]).await.unwrap();
    f.call_batch(&mut tx, batch, "action_confirm")
        .await
        .unwrap();
    f.call_batch(&mut tx, batch, "action_done").await.unwrap();
    assert_eq!(f.queued(&mut tx, batch, 1).await["run"], 1);
    // live job: no joining, no cancel, no second validation, no availability check
    let error = f.add(&mut tx, batch, &[b]).await.unwrap_err();
    assert!(
        error.message().contains("being validated"),
        "{}",
        error.message()
    );
    for method in ["action_cancel", "action_done", "action_assign"] {
        f.call_batch(&mut tx, batch, method).await.unwrap_err();
    }
    // the job dies: the flag no longer holds the batch
    tx.execute(
        "UPDATE erp_job SET state = 'dead', dead_at = now() WHERE dedup_key = $1",
        &[&format!("stock.batch_validate:{batch}:1")],
    )
    .await
    .unwrap();
    f.call_batch(&mut tx, batch, "action_done").await.unwrap();
    assert_eq!(
        batch_row(&mut tx, batch).await.2,
        1,
        "the same run continues"
    );
    assert_eq!(f.queued(&mut tx, batch, 1).await["id"], batch);
    tx.execute(
        "UPDATE erp_job SET state = 'dead', dead_at = now() WHERE dedup_key = $1",
        &[&format!("stock.batch_validate:{batch}:1")],
    )
    .await
    .unwrap();
    f.call_batch(&mut tx, batch, "action_cancel").await.unwrap();
    let row = batch_row(&mut tx, batch).await;
    assert_eq!((row.0.as_str(), row.1), ("cancel", false));
}

/// Validate settles a batch whose transfers were all finished on their own, and refuses a batch
/// where nothing is reserved instead of emptying it.
#[tokio::test]
async fn validate_settles_finished_batches_and_refuses_empty_ones() {
    let f = fixture().await;
    let mut tx = f.h.begin().await.unwrap();
    // a delivery nothing is reserved for (made before the receipt puts stock on hand)
    let waiting = f.transfer(&mut tx, false, "1").await;
    let a = f.transfer(&mut tx, true, "1").await;
    let batch = f.batch(&mut tx).await.unwrap();
    f.add(&mut tx, batch, &[a]).await.unwrap();
    f.call_batch(&mut tx, batch, "action_confirm")
        .await
        .unwrap();
    f.call(&mut tx, a, "button_validate").await.unwrap();
    f.call_batch(&mut tx, batch, "action_done").await.unwrap();
    assert_eq!(batch_row(&mut tx, batch).await.0, "done");

    let other = f.batch(&mut tx).await.unwrap();
    f.add(&mut tx, other, &[waiting]).await.unwrap();
    f.call_batch(&mut tx, other, "action_confirm")
        .await
        .unwrap();
    let error = f
        .call_batch(&mut tx, other, "action_done")
        .await
        .unwrap_err();
    assert!(
        error.message().contains("Check Availability"),
        "{}",
        error.message()
    );
    let row = batch_row(&mut tx, other).await;
    assert_eq!((row.0.as_str(), row.1), ("in_progress", false));
    assert_eq!(picking_batch(&mut tx, waiting).await, Some(other));
}
