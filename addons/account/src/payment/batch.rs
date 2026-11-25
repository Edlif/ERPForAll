// SPDX-License-Identifier: MIT OR Apache-2.0
//! Batch Payments (`account.batch.payment`) and exact-decimal check amount-in-words helper.

use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::payment::{post_payments, service::validate_payments};

/// `account.batch.payment`: groups inbound or outbound payments on one bank/cash journal
/// for deposit slips or batch bank remittances.
#[model(
    name = "account.batch.payment",
    description = "Batch Payment",
    order = "date desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountBatchPayment {
    /// Auto-numbered reference (`BATCH/IN/00001` or `BATCH/OUT/00001`).
    #[field(
        char,
        required = true,
        readonly = true,
        copy = false,
        default = "New",
        string = "Reference"
    )]
    pub name: Option<Arc<str>>,
    /// Batch date.
    #[field(date, required = true, default_fn = default_today, string = "Date")]
    pub date: Option<Date>,
    /// Inbound customer deposit or outbound vendor batch.
    #[field(
        selection,
        selection = [("inbound", "Inbound"), ("outbound", "Outbound")],
        required = true,
        default = "inbound",
        string = "Batch Type"
    )]
    pub batch_type: Option<Arc<str>>,
    /// Workflow status.
    #[field(
        selection,
        selection = [
            ("draft", "New"),
            ("sent", "Sent"),
            ("reconciled", "Reconciled"),
        ],
        required = true,
        readonly = true,
        copy = false,
        default = "draft",
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
    /// Bank or cash journal of the batch.
    #[field(
        many2one,
        comodel = "account.journal",
        required = true,
        check_company = true,
        ondelete = "restrict",
        domain = [("type", "in", ["bank", "cash", "credit"])],
        string = "Bank"
    )]
    pub journal_id: Option<RecId>,
    /// Company derived from the journal.
    #[field(
        many2one,
        comodel = "res.company",
        related = "journal_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Currency derived from the journal or its company.
    #[field(
        many2one,
        comodel = "res.currency",
        compute = "_compute_amount",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Payments included in this batch.
    #[field(
        one2many,
        comodel = "account.payment",
        inverse_name = "batch_payment_id",
        string = "Payments"
    )]
    pub payment_ids: Vec<RecId>,
    /// Total exact decimal amount of payments in the batch.
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_amount",
        store = true,
        string = "Total"
    )]
    pub amount: Decimal,
    /// Number of payments in the batch.
    #[field(
        integer,
        compute = "_compute_amount",
        store = true,
        string = "Payments Count"
    )]
    pub payment_count: i32,
    /// Optional bank deposit slip or remittance memo.
    #[field(char, string = "Memo")]
    pub memo: Option<Arc<str>>,
}

fn default_today(_ctx: &erp_sdk::DefaultContext) -> Value {
    Value::Date(Date::today())
}

#[methods(AccountBatchPayment, addon = "account")]
impl AccountBatchPayment {
    #[compute(depends = ["journal_id", "payment_ids", "payment_ids.amount"])]
    async fn _compute_amount(
        this: &RecordSet<AccountBatchPayment>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountBatchPayment>,
    ) -> Result<()> {
        let journals = this.mapped(tx, AccountBatchPayment::JOURNAL_ID).await?;
        let rec_ids = this.as_records().rec_ids();
        for (i, (rec_id, journal)) in rec_ids.iter().zip(journals).enumerate() {
            let mut currency_id = None;
            if let Some(jid) = journal {
                let rows = tx
                    .query(
                        "SELECT COALESCE(j.currency_id, c.currency_id)::int8 \
                         FROM account_journal j JOIN res_company c ON c.id = j.company_id \
                         WHERE j.id = $1",
                        &[&jid.get()],
                    )
                    .await?;
                if let Some(row) = rows.first() {
                    let cid: Option<i64> = row
                        .try_get(0)
                        .map_err(|e| Error::db(format!("Batch currency unreadable: {e}")))?;
                    currency_id = cid.and_then(RecId::from_real);
                }
            }
            let (total, count) = if let Some(real) = rec_id.as_real() {
                let rows = tx
                    .query(
                        "SELECT COALESCE(SUM(amount), 0), count(*)::int4 \
                         FROM account_payment \
                         WHERE batch_payment_id = $1 AND state <> 'canceled'",
                        &[&real.get()],
                    )
                    .await?;
                if let Some(row) = rows.first() {
                    let sum: Decimal = row
                        .try_get(0)
                        .map_err(|e| Error::db(format!("Batch total unreadable: {e}")))?;
                    let cnt: i32 = row
                        .try_get(1)
                        .map_err(|e| Error::db(format!("Batch count unreadable: {e}")))?;
                    (sum, cnt)
                } else {
                    (Decimal::ZERO, 0)
                }
            } else {
                (Decimal::ZERO, 0)
            };
            out.set(i, AccountBatchPayment::CURRENCY_ID, currency_id)?;
            out.set(i, AccountBatchPayment::AMOUNT, total)?;
            out.set(i, AccountBatchPayment::PAYMENT_COUNT, count)?;
        }
        Ok(())
    }

    #[method(override)]
    async fn create(
        this: &RecordSet<AccountBatchPayment>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        for vals in &mut vals_list {
            let needs_name = match vals.get("name") {
                None | Some(Cell::Null) => true,
                Some(Cell::Str(s)) => s.trim().is_empty() || s.as_ref() == "New",
                _ => false,
            };
            if needs_name {
                let batch_type = match vals.get("batch_type") {
                    Some(Cell::Str(s)) if s.as_ref() == "outbound" => "outbound",
                    _ => "inbound",
                };
                let prefix = if batch_type == "outbound" {
                    "BATCH/OUT/"
                } else {
                    "BATCH/IN/"
                };
                let rows = tx
                    .query(
                        "SELECT count(*)::int8 + 1 FROM account_batch_payment WHERE batch_type = $1",
                        &[&batch_type],
                    )
                    .await?;
                let next: i64 = rows.first().and_then(|r| r.try_get(0).ok()).unwrap_or(1);
                vals.insert(
                    "name".into(),
                    Cell::Str(format!("{prefix}{next:05}").into()),
                );
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }

    /// Validates the batch: verifies at least one payment, matching direction/journal, posts
    /// any draft payments, and marks the batch Sent.
    #[method]
    #[rpc(effect = write)]
    pub async fn validate_batch(
        this: &RecordSet<AccountBatchPayment>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        let env = this.env().clone();
        for batch in this.iter() {
            let bid = batch.as_records().ids()[0];
            let state = batch.get(tx, AccountBatchPayment::STATE).await?;
            if state.as_deref() != Some("draft") {
                return Err(Error::user("Only draft batch payments can be validated."));
            }
            let journal_id = batch
                .get(tx, AccountBatchPayment::JOURNAL_ID)
                .await?
                .ok_or_else(|| Error::validation("Choose a bank journal for the batch payment."))?
                .get();
            let batch_type = batch
                .get(tx, AccountBatchPayment::BATCH_TYPE)
                .await?
                .unwrap_or_else(|| Arc::from("inbound"));
            let rows = tx
                .query(
                    "SELECT id::int8, state::text, journal_id::int8, payment_type::text \
                     FROM account_payment WHERE batch_payment_id = $1 ORDER BY id",
                    &[&bid],
                )
                .await?;
            if rows.is_empty() {
                return Err(Error::user(
                    "Add at least one payment before validating this batch.",
                ));
            }
            let mut draft_ids = Vec::new();
            for row in &rows {
                let pid: i64 = row
                    .try_get(0)
                    .map_err(|e| Error::db(format!("Payment id unreadable: {e}")))?;
                let pstate: String = row
                    .try_get(1)
                    .map_err(|e| Error::db(format!("Payment state unreadable: {e}")))?;
                let pjournal: i64 = row
                    .try_get(2)
                    .map_err(|e| Error::db(format!("Payment journal unreadable: {e}")))?;
                let ptype: String = row
                    .try_get(3)
                    .map_err(|e| Error::db(format!("Payment type unreadable: {e}")))?;
                if pjournal != journal_id {
                    return Err(Error::user(
                        "All payments in a batch must belong to the batch's bank journal.",
                    ));
                }
                if ptype != batch_type.as_ref() {
                    return Err(Error::user(
                        "All payments in a batch must match the batch direction (Inbound/Outbound).",
                    ));
                }
                if pstate == "canceled" || pstate == "rejected" {
                    return Err(Error::user(
                        "Cancelled or rejected payments cannot be validated in a batch.",
                    ));
                }
                if pstate == "draft" {
                    draft_ids.push(pid);
                }
            }
            if !draft_ids.is_empty() {
                let drafts = env.records("account.payment")?.browse(draft_ids)?;
                post_payments(tx, &drafts).await?;
            }
            tx.execute(
                "UPDATE account_batch_payment SET state = 'sent', write_date = now() AT TIME ZONE 'UTC' \
                 WHERE id = $1",
                &[&bid],
            )
            .await?;
            tx.invalidate_records(
                batch.as_records(),
                Some(&["state", "amount", "payment_count"]),
            )
            .await?;
        }
        Ok(())
    }

    /// Marks every Paid payment in a Sent batch as Reconciled and transitions the batch to Reconciled.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_reconcile_batch(
        this: &RecordSet<AccountBatchPayment>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        let env = this.env().clone();
        for batch in this.iter() {
            let bid = batch.as_records().ids()[0];
            let state = batch.get(tx, AccountBatchPayment::STATE).await?;
            if state.as_deref() != Some("sent") {
                return Err(Error::user("Only sent batch payments can be reconciled."));
            }
            let rows = tx
                .query(
                    "SELECT id::int8 FROM account_payment \
                     WHERE batch_payment_id = $1 AND state = 'paid' ORDER BY id",
                    &[&bid],
                )
                .await?;
            let paid_ids: Vec<i64> = rows
                .iter()
                .map(|r| {
                    r.try_get::<_, i64>(0)
                        .map_err(|e| Error::db(format!("Batch payment id unreadable: {e}")))
                })
                .collect::<Result<_>>()?;
            if !paid_ids.is_empty() {
                let paid = env.records("account.payment")?.browse(paid_ids)?;
                validate_payments(tx, &paid).await?;
            }
            tx.execute(
                "UPDATE account_batch_payment SET state = 'reconciled', write_date = now() AT TIME ZONE 'UTC' \
                 WHERE id = $1",
                &[&bid],
            )
            .await?;
            tx.invalidate_records(batch.as_records(), Some(&["state"]))
                .await?;
        }
        Ok(())
    }

    /// Resets a Sent batch back to Draft (reconciled batches remain locked).
    #[method]
    #[rpc(effect = write)]
    pub async fn action_reset_draft(
        this: &RecordSet<AccountBatchPayment>,
        tx: &mut Transaction,
    ) -> Result<()> {
        this.as_records().check_access(tx, Operation::Write).await?;
        for batch in this.iter() {
            let bid = batch.as_records().ids()[0];
            let state = batch.get(tx, AccountBatchPayment::STATE).await?;
            if state.as_deref() != Some("sent") {
                return Err(Error::user(
                    "Only sent batch payments can be reset to draft.",
                ));
            }
            tx.execute(
                "UPDATE account_batch_payment SET state = 'draft', write_date = now() AT TIME ZONE 'UTC' \
                 WHERE id = $1",
                &[&bid],
            )
            .await?;
            tx.invalidate_records(batch.as_records(), Some(&["state"]))
                .await?;
        }
        Ok(())
    }
}

/// Converts an exact `Decimal` monetary amount to English words for check printing and
/// amount-in-words display (never uses floating-point arithmetic).
pub fn amount_to_words(amount: Decimal, currency_code: &str) -> String {
    let abs = amount.abs().round_dp(2);
    let whole_dec = abs.trunc();
    let frac_dec = ((abs - whole_dec) * Decimal::from(100)).round_dp(0);
    let whole: u64 = whole_dec.to_string().parse().unwrap_or(0);
    let cents: u64 = frac_dec.to_string().parse().unwrap_or(0);

    let code = currency_code.to_uppercase();
    let (major_sg, major_pl, minor_sg, minor_pl) = match code.as_str() {
        "USD" => ("Dollar", "Dollars", "Cent", "Cents"),
        "EUR" => ("Euro", "Euros", "Cent", "Cents"),
        "GBP" => ("Pound", "Pounds", "Penny", "Pence"),
        "IDR" => ("Rupiah", "Rupiah", "Sen", "Sen"),
        "SGD" => ("Singapore Dollar", "Singapore Dollars", "Cent", "Cents"),
        other if !other.is_empty() => (other, other, "Cent", "Cents"),
        _ => ("Unit", "Units", "Cent", "Cents"),
    };

    let major_word = integer_to_words(whole);
    let major_label = if whole == 1 { major_sg } else { major_pl };
    let prefix = if amount.is_sign_negative() && !abs.is_zero() {
        "Minus "
    } else {
        ""
    };
    if cents == 0 {
        format!("{prefix}{major_word} {major_label}")
    } else {
        let minor_word = integer_to_words(cents);
        let minor_label = if cents == 1 { minor_sg } else { minor_pl };
        format!("{prefix}{major_word} {major_label} and {minor_word} {minor_label}")
    }
}

fn integer_to_words(n: u64) -> String {
    if n == 0 {
        return "Zero".to_owned();
    }
    const SCALES: [&str; 5] = ["", "Thousand", "Million", "Billion", "Trillion"];
    let mut parts = Vec::new();
    let mut rem = n;
    let mut scale = 0usize;
    while rem > 0 {
        let chunk = rem % 1000;
        if chunk > 0 {
            let chunk_str = chunk_to_words(chunk);
            if scale > 0 && scale < SCALES.len() {
                parts.push(format!("{chunk_str} {}", SCALES[scale]));
            } else {
                parts.push(chunk_str);
            }
        }
        rem /= 1000;
        scale += 1;
    }
    parts.reverse();
    parts.join(" ")
}

fn chunk_to_words(n: u64) -> String {
    const ONES: [&str; 20] = [
        "",
        "One",
        "Two",
        "Three",
        "Four",
        "Five",
        "Six",
        "Seven",
        "Eight",
        "Nine",
        "Ten",
        "Eleven",
        "Twelve",
        "Thirteen",
        "Fourteen",
        "Fifteen",
        "Sixteen",
        "Seventeen",
        "Eighteen",
        "Nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "Twenty", "Thirty", "Forty", "Fifty", "Sixty", "Seventy", "Eighty", "Ninety",
    ];
    let mut out = Vec::new();
    let hundreds = n / 100;
    let rem = n % 100;
    if hundreds > 0 {
        out.push(format!("{} Hundred", ONES[hundreds as usize]));
    }
    if rem >= 20 {
        let t = TENS[(rem / 10) as usize];
        let o = ONES[(rem % 10) as usize];
        if o.is_empty() {
            out.push(t.to_owned());
        } else {
            out.push(format!("{t}-{o}"));
        }
    } else if rem > 0 {
        out.push(ONES[rem as usize].to_owned());
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::amount_to_words;
    use erp_sdk::Decimal;
    use std::str::FromStr;

    #[test]
    fn exact_decimal_amount_to_words_formats_dollars_and_cents() {
        let amt = Decimal::from_str("1234.56").expect("valid decimal");
        assert_eq!(
            amount_to_words(amt, "USD"),
            "One Thousand Two Hundred Thirty-Four Dollars and Fifty-Six Cents"
        );
    }

    #[test]
    fn exact_decimal_amount_to_words_handles_zero_and_idr() {
        assert_eq!(amount_to_words(Decimal::ZERO, "USD"), "Zero Dollars");
        let idr = Decimal::from_str("1500000").expect("valid decimal");
        assert_eq!(
            amount_to_words(idr, "IDR"),
            "One Million Five Hundred Thousand Rupiah"
        );
    }
}
