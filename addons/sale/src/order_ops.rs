// SPDX-License-Identifier: MIT OR Apache-2.0
//! The operations of a sales order and the seams of its confirm and cancel chains (B2,
//! "States and every legal transition", D4). Every operation acts on the whole record set: it
//! takes the row locks in id order, reads the states inside the transaction, refuses the whole
//! call when one order is not in a legal source state, and writes `state`, `locked` and the
//! confirmation time through the base write, as the caller.

use std::collections::BTreeSet;

use erp_sdk::prelude::*;

use crate::order::{Header, SaleOrder, headers};

/// The posted customer invoices and credit notes of the orders: order id, order number, invoice
/// number, the first per order first.
const POSTED_INVOICES: &str = "SELECT DISTINCT o.id::int8, o.name, m.name FROM sale_invoice_item k \
    JOIN sale_order_line s ON s.id = k.sale_line_id \
    JOIN sale_order o ON o.id = s.order_id \
    JOIN account_move_line l ON l.id = k.invoice_item_id \
    JOIN account_move m ON m.id = l.move_id \
    WHERE s.order_id = ANY($1) AND m.state = 'posted' \
    AND m.move_type IN ('out_invoice', 'out_refund') ORDER BY 1, 3";

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading the invoices of a sales order failed.").with_source(error)
}

/// The stored ids of `records`; a new record has no invoice yet.
fn stored(records: &Records) -> Vec<i64> {
    records
        .rec_ids()
        .iter()
        .filter_map(|id| id.as_real().map(|id| id.get()))
        .collect()
}

/// The group that may lock and unlock orders (V6).
pub const LOCK_GROUP: &str = "sales_team.group_sale_manager";

/// Lock the orders and read their headers (rule 1 of every operation).
async fn locked_headers(tx: &mut Transaction, records: &Records) -> Result<Vec<Header>> {
    records.lock_for_update(tx, false).await?;
    headers(tx, records).await
}

/// The orders of `orders` that `keep` refuses, described with their state.
fn named(orders: &[Header], refuse: impl Fn(&Header) -> bool) -> Option<String> {
    let refused: Vec<String> = orders
        .iter()
        .filter(|order| refuse(order))
        .map(Header::describe)
        .collect();
    (!refused.is_empty()).then(|| refused.join(", "))
}

fn browse(records: &Records, orders: &[Header], keep: impl Fn(&Header) -> bool) -> Records {
    records.browse_ids(
        orders
            .iter()
            .filter(|order| keep(order))
            .map(|order| order.id),
    )
}

fn state(value: &str) -> RawVals {
    RawVals::new().set_cell("state", Cell::Str(value.into()))
}

fn lock_value(locked: bool) -> RawVals {
    RawVals::new().set_cell("locked", Cell::Bool(locked))
}

/// Superuser mode passes, as the other group-gated operations do (`account` posting).
async fn check_lock_group(tx: &mut Transaction, records: &Records) -> Result<()> {
    let env = records.env();
    if env.su() || tx.has_group(env, LOCK_GROUP).await? {
        Ok(())
    } else {
        Err(Error::access(
            "Only a Sales Administrator can lock or unlock sales orders.",
        ))
    }
}

#[methods(SaleOrder, addon = "sale")]
impl SaleOrder {
    /// Marks draft quotations as sent.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_quotation_sent(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        let orders = locked_headers(tx, records).await?;
        if let Some(refused) = named(&orders, |order| order.state != "draft") {
            return Err(Error::validation(format!(
                "A quotation is marked as sent only while it is a draft: {refused}."
            )));
        }
        records.write_base(tx, state("sent")).await
    }

    /// Confirms quotations: the refusal check, the confirmation values, `_action_confirm`, then
    /// the lock of the orders `_should_be_locked` returns ("The confirm chain";
    /// ORM caller can handle a late refusal without committing partial confirmation.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_confirm(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        tx.savepoint(async move |tx| {
            locked_headers(tx, records).await?;
            let refusal: Option<String> =
                records.call(tx, "_confirmation_error_message", ()).await?;
            if let Some(refusal) = refusal {
                return Err(Error::validation(refusal));
            }
            let values: Vals<SaleOrder> =
                records.call(tx, "_prepare_confirmation_values", ()).await?;
            records.write_base(tx, values.into_raw()).await?;
            let () = records.call(tx, "_action_confirm", ()).await?;
            let to_lock: Records = records.call(tx, "_should_be_locked", ()).await?;
            if !to_lock.is_empty() {
                to_lock.write_base(tx, lock_value(true)).await?;
            }
            Ok(())
        })
        .await
    }

    /// Cancels the orders not yet cancelled through `_action_cancel`; a locked order refuses the
    /// whole call. Already cancelled orders are left as they are.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_cancel(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        let orders = locked_headers(tx, records).await?;
        if let Some(refused) = named(&orders, |order| order.locked) {
            return Err(Error::user(format!(
                "A locked sales order cannot be cancelled; unlock it first: {refused}."
            )));
        }
        let open = browse(records, &orders, |order| order.state != "cancel");
        if open.is_empty() {
            return Ok(());
        }
        open.call(tx, "_action_cancel", ()).await
    }

    /// Sets sent quotations and cancelled orders back to draft; any other state refuses the whole
    /// call.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_draft(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        let orders = locked_headers(tx, records).await?;
        if let Some(refused) = named(&orders, |order| {
            !matches!(order.state.as_str(), "sent" | "cancel")
        }) {
            return Err(Error::validation(format!(
                "Only a sent quotation or a cancelled order can be set back to draft: {refused}."
            )));
        }
        records.write_base(tx, state("draft")).await
    }

    /// Locks confirmed orders; Sales administrators only.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_lock(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        check_lock_group(tx, records).await?;
        let orders = locked_headers(tx, records).await?;
        if let Some(refused) = named(&orders, |order| order.state != "sale") {
            return Err(Error::validation(format!(
                "Only a confirmed sales order can be locked: {refused}."
            )));
        }
        let open = browse(records, &orders, |order| !order.locked);
        if open.is_empty() {
            return Ok(());
        }
        open.write_base(tx, lock_value(true)).await
    }

    /// Unlocks locked orders; Sales administrators only.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_unlock(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        check_lock_group(tx, records).await?;
        let orders = locked_headers(tx, records).await?;
        let locked = browse(records, &orders, |order| order.locked);
        if locked.is_empty() {
            return Ok(());
        }
        locked.write_base(tx, lock_value(false)).await
    }

    /// The first refusal of a confirmation, or `None`: in the base, an order that is neither a
    /// draft nor a sent quotation.
    #[method]
    async fn _confirmation_error_message(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Option<String>> {
        let orders = headers(tx, this.as_records()).await?;
        Ok(named(&orders, |order| {
            !matches!(order.state.as_str(), "draft" | "sent")
        })
        .map(|refused| format!("Only a draft or sent quotation can be confirmed: {refused}.")))
    }

    /// The values a confirmation writes: the state and the confirmation time.
    /// An override adds its own through field handles and keeps
    /// these (R7).
    #[method]
    async fn _prepare_confirmation_values(
        _this: &RecordSet<SaleOrder>,
        _tx: &mut Transaction,
    ) -> Result<Vals<SaleOrder>> {
        Ok(Vals::new()
            .set(SaleOrder::STATE, Some(Arc::<str>::from("sale")))
            .set(SaleOrder::DATE_ORDER, Some(DateTime::now())))
    }

    /// The step of a confirmation where satellites act; empty in the base.
    #[method]
    async fn _action_confirm(_this: &RecordSet<SaleOrder>, _tx: &mut Transaction) -> Result<()> {
        Ok(())
    }

    /// The orders a confirmation locks: all of them when "Lock confirmed sales" is on, none when
    /// it is off (B4).
    #[method]
    async fn _should_be_locked(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Records> {
        if crate::settings::lock_confirmed(tx).await? {
            Ok(this.as_records().clone())
        } else {
            Ok(this.as_records().browse([])?)
        }
    }

    /// The cancel step: in the base, an order with a posted invoice or credit note is refused
    /// by name, otherwise the state becomes `cancel`. Stricter than the reference, which cancels
    /// the order and leaves the posted invoice: what was invoiced stays recorded on the order.
    #[method]
    async fn _action_cancel(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<()> {
        let records = this.as_records();
        tx.flush().await?;
        if let Some(row) = tx.query(POSTED_INVOICES, &[&stored(records)]).await?.first() {
            let order: String = row.try_get(1).map_err(decode)?;
            let invoice: Option<String> = row.try_get(2).map_err(decode)?;
            return Err(Error::user(format!(
                "The sales order {order} cannot be cancelled: its invoice {} is posted. Correct the invoice with a credit note; the order keeps what was invoiced.",
                invoice.as_deref().unwrap_or("(no number)")
            )));
        }
        records.write_base(tx, state("cancel")).await
    }

    /// The orders whose Cancel would be refused: in the base, those with a posted invoice or
    /// credit note. A bridge that refuses the cancel step adds its own through an override that
    /// calls `sup`.
    #[method]
    async fn _cancel_blocked(this: &RecordSet<SaleOrder>, tx: &mut Transaction) -> Result<Records> {
        let records = this.as_records();
        tx.flush().await?;
        let mut blocked = BTreeSet::new();
        for row in tx.query(POSTED_INVOICES, &[&stored(records)]).await? {
            blocked.insert(row.try_get::<_, i64>(0).map_err(decode)?);
        }
        records.browse(blocked)
    }

    #[compute(depends = [])]
    async fn _compute_cancel_blocked(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
        out: &mut Assign<'_, SaleOrder>,
    ) -> Result<()> {
        let records = this.as_records();
        let stored = records.browse(stored(records))?;
        let blocked: Records = stored.call(tx, "_cancel_blocked", ()).await?;
        let blocked: BTreeSet<i64> = blocked.ids().into_iter().collect();
        for (index, id) in records.rec_ids().iter().enumerate() {
            let held = id.as_real().is_some_and(|id| blocked.contains(&id.get()));
            out.set(index, SaleOrder::CANCEL_BLOCKED, held)?;
        }
        Ok(())
    }
}
