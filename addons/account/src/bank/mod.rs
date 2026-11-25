// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bank Statements (`account.bank.statement`) and Bank Reconciliation
//! (`account.bank.statement.line`).

use std::str::FromStr;

use erp_sdk::Operation;
use erp_sdk::prelude::*;

use crate::moves::post_moves;
use crate::reconcile::reconcile_lines;

/// One parsed row from a bank statement CSV snippet (`date,label,amount[,ref]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvStatementRow {
    pub date: Date,
    pub label: String,
    pub amount: Decimal,
    pub reference: Option<String>,
}

/// Pure exact-decimal parser for bank statement CSV lines (`YYYY-MM-DD,Label,Amount[,Reference]`).
/// Skips empty lines and an optional header row starting with `date`.
pub fn parse_bank_statement_csv(raw: &str) -> Result<Vec<CsvStatementRow>> {
    let mut rows = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let cols: Vec<&str> = trimmed.split(',').map(str::trim).collect();
        if idx == 0 && cols.first().is_some_and(|c| c.eq_ignore_ascii_case("date")) {
            continue;
        }
        if cols.len() < 3 {
            return Err(Error::user(format!(
                "CSV line {} must have at least 3 columns (date,label,amount): {trimmed}",
                idx + 1
            )));
        }
        let date = Date::parse_odoo(cols[0]).ok_or_else(|| {
            Error::user(format!(
                "CSV line {} has an invalid date {:?}; expected YYYY-MM-DD.",
                idx + 1,
                cols[0]
            ))
        })?;
        let label = cols[1].to_owned();
        if label.is_empty() {
            return Err(Error::user(format!(
                "CSV line {} has an empty label.",
                idx + 1
            )));
        }
        let amount = Decimal::from_str(cols[2]).map_err(|_| {
            Error::user(format!(
                "CSV line {} has an invalid decimal amount {:?}.",
                idx + 1,
                cols[2]
            ))
        })?;
        let reference = cols
            .get(3)
            .map(|s| (*s).to_owned())
            .filter(|s| !s.is_empty());
        rows.push(CsvStatementRow {
            date,
            label,
            amount,
            reference,
        });
    }
    if rows.is_empty() {
        return Err(Error::user(
            "Provide at least one transaction row (YYYY-MM-DD,Label,Amount).",
        ));
    }
    Ok(rows)
}

/// Pure exact-decimal balance split for one bank statement line.
/// Positive `amount` (bank inflow) debits the bank account (`+amount`) and credits the counterpart
/// (`-amount`). Negative `amount` (bank outflow) credits the bank account and debits counterpart.
pub fn compute_statement_entry_balances(amount: Decimal) -> (Decimal, Decimal) {
    (amount, -amount)
}

fn default_today(_ctx: &erp_sdk::DefaultContext) -> Value {
    Value::Date(Date::today())
}

fn db(err: impl std::fmt::Display) -> Error {
    Error::db(format!("Bank statement query failed: {err}"))
}

/// `account.bank.statement`: periodic bank or cash statement grouping statement lines.
#[model(
    name = "account.bank.statement",
    description = "Bank Statement",
    order = "date desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountBankStatement {
    /// Auto-numbered statement reference (`BNKST/2026/00001`).
    #[field(
        char,
        required = true,
        default = "New",
        copy = false,
        string = "Reference"
    )]
    pub name: Option<Arc<str>>,
    /// Bank, cash or credit-card journal.
    #[field(
        many2one,
        comodel = "account.journal",
        required = true,
        check_company = true,
        ondelete = "restrict",
        domain = [("type", "in", ["bank", "cash", "credit"])],
        string = "Journal"
    )]
    pub journal_id: Option<RecId>,
    /// Owning company from the journal.
    #[field(
        many2one,
        comodel = "res.company",
        related = "journal_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Currency from the journal or its company.
    #[field(
        many2one,
        comodel = "res.currency",
        compute = "_compute_currency",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Statement date.
    #[field(date, required = true, default_fn = default_today, string = "Date")]
    pub date: Option<Date>,
    /// Opening balance carried from the previous statement.
    #[field(monetary, currency_field = "currency_id", string = "Starting Balance")]
    pub balance_start: Decimal,
    /// Closing balance reported by the bank.
    #[field(monetary, currency_field = "currency_id", string = "Ending Balance")]
    pub balance_end_real: Decimal,
    /// Computed closing balance (`balance_start + sum(line_ids.amount)`).
    #[field(
        monetary,
        currency_field = "currency_id",
        compute = "_compute_balances",
        store = true,
        string = "Computed Balance"
    )]
    pub balance_end: Decimal,
    /// True when `balance_end == balance_end_real`.
    #[field(
        boolean,
        compute = "_compute_balances",
        store = true,
        string = "Balance Matches"
    )]
    pub is_complete: bool,
    /// Count of statement lines not yet reconciled.
    #[field(
        integer,
        compute = "_compute_balances",
        store = true,
        string = "Lines To Reconcile"
    )]
    pub unreconciled_count: i32,
    /// Open while reconciling; Posted once balanced and reconciled.
    #[field(
        selection,
        selection = [("open", "Open"), ("posted", "Posted")],
        required = true,
        readonly = true,
        copy = false,
        default = "open",
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
    /// Statement transactions.
    #[field(
        one2many,
        comodel = "account.bank.statement.line",
        inverse_name = "statement_id",
        string = "Transactions"
    )]
    pub line_ids: Vec<RecId>,
    /// Optional pasted CSV rows (`YYYY-MM-DD,Label,Amount[,Reference]`) imported via button.
    #[field(text, string = "Import CSV (date,label,amount,ref)")]
    pub csv_import_data: Option<Arc<str>>,
}

#[methods(AccountBankStatement, addon = "account")]
impl AccountBankStatement {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountBankStatement>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<erp_sdk::ValueMap> {
        let mut values: erp_sdk::ValueMap =
            sup.call(tx, this.as_records(), (names.clone(),)).await?;
        let env = this.env();
        if let Some(cid) = env.company_id()?
            && names.iter().any(|n| n == "journal_id")
            && !values.contains_key("journal_id")
        {
            let jrows = tx
                .query(
                    "SELECT id::int8 FROM account_journal \
                     WHERE company_id = $1 AND type IN ('bank', 'cash') AND active = true \
                     ORDER BY CASE type WHEN 'bank' THEN 0 ELSE 1 END, sequence, id LIMIT 1",
                    &[&cid],
                )
                .await?;
            if let Some(jid) = jrows.first().and_then(|r| r.try_get::<_, i64>(0).ok()) {
                values.insert("journal_id".to_owned(), Value::Int(jid));
            }
        }
        Ok(values)
    }

    #[compute(candidate, depends = ["journal_id"])]
    async fn _compute_currency(
        this: &RecordSet<AccountBankStatement>,
        tx: &mut ReadTx<'_>,
        out: &mut Assign<'_, AccountBankStatement>,
    ) -> Result<()> {
        let journals = tx.mapped(this, AccountBankStatement::JOURNAL_ID).await?;
        for (i, journal) in journals.into_iter().enumerate() {
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
                    let cid: Option<i64> = row.try_get(0).map_err(db)?;
                    currency_id = cid.and_then(RecId::from_real);
                }
            }
            out.set(i, AccountBankStatement::CURRENCY_ID, currency_id)?;
        }
        Ok(())
    }

    #[compute(depends = [
        "balance_start",
        "balance_end_real",
        "line_ids",
        "line_ids.amount",
        "line_ids.is_reconciled",
    ])]
    async fn _compute_balances(
        this: &RecordSet<AccountBankStatement>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountBankStatement>,
    ) -> Result<()> {
        let starts = this.mapped(tx, AccountBankStatement::BALANCE_START).await?;
        let reals = this
            .mapped(tx, AccountBankStatement::BALANCE_END_REAL)
            .await?;
        let rec_ids = this.as_records().rec_ids();
        for (i, ((rec_id, start), real_end)) in rec_ids.iter().zip(starts).zip(reals).enumerate() {
            let (line_sum, unrec_count) = if let Some(real) = rec_id.as_real() {
                let rows = tx
                    .query(
                        "SELECT COALESCE(SUM(amount), 0), \
                                count(*) FILTER (WHERE NOT COALESCE(is_reconciled, false))::int4 \
                         FROM account_bank_statement_line WHERE statement_id = $1",
                        &[&real.get()],
                    )
                    .await?;
                if let Some(row) = rows.first() {
                    (
                        row.try_get::<_, Decimal>(0).map_err(db)?,
                        row.try_get::<_, i32>(1).map_err(db)?,
                    )
                } else {
                    (Decimal::ZERO, 0)
                }
            } else {
                (Decimal::ZERO, 0)
            };
            let computed_end = start + line_sum;
            let matches = computed_end == real_end;
            out.set(i, AccountBankStatement::BALANCE_END, computed_end)?;
            out.set(i, AccountBankStatement::IS_COMPLETE, matches)?;
            out.set(i, AccountBankStatement::UNRECONCILED_COUNT, unrec_count)?;
        }
        Ok(())
    }

    #[method(override)]
    async fn create(
        this: &RecordSet<AccountBankStatement>,
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
                let year = match vals.get("date") {
                    Some(Cell::Date(d)) => d.ymd().0,
                    _ => Date::today().ymd().0,
                };
                let rows = tx
                    .query("SELECT count(*)::int8 + 1 FROM account_bank_statement", &[])
                    .await?;
                let next: i64 = rows.first().and_then(|r| r.try_get(0).ok()).unwrap_or(1);
                vals.insert(
                    "name".into(),
                    Cell::Str(format!("BNKST/{year}/{next:05}").into()),
                );
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }

    /// Parses pasted CSV rows (`YYYY-MM-DD,Label,Amount[,Ref]`) and appends them as statement lines.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_import_csv(
        this: &RecordSet<AccountBankStatement>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let stmt = this.ensure_one()?;
        stmt.as_records().check_access(tx, Operation::Write).await?;
        let state = stmt.get(tx, AccountBankStatement::STATE).await?;
        if state.as_deref() != Some("open") {
            return Err(Error::user(
                "Only open statements can import new CSV transactions.",
            ));
        }
        let raw = stmt
            .get(tx, AccountBankStatement::CSV_IMPORT_DATA)
            .await?
            .unwrap_or_default();
        let parsed = parse_bank_statement_csv(&raw)?;
        let sid = stmt.as_records().ids()[0];
        let journal_id = stmt
            .get(tx, AccountBankStatement::JOURNAL_ID)
            .await?
            .ok_or_else(|| Error::validation("Choose a journal for this bank statement."))?
            .get();
        let mut line_vals = Vec::with_capacity(parsed.len());
        for row in parsed {
            let mut v = RawVals::new();
            v.insert("statement_id".into(), Cell::Id(sid));
            v.insert("journal_id".into(), Cell::Id(journal_id));
            v.insert("date".into(), Cell::Date(row.date));
            v.insert("payment_ref".into(), Cell::Str(row.label.into()));
            v.insert("amount".into(), Cell::Decimal(row.amount));
            if let Some(r) = row.reference {
                v.insert("ref".into(), Cell::Str(r.into()));
            }
            line_vals.push(v);
        }
        stmt.env()
            .records("account.bank.statement.line")?
            .create(tx, line_vals)
            .await?;
        tx.execute(
            "UPDATE account_bank_statement SET csv_import_data = NULL, \
             write_date = now() AT TIME ZONE 'UTC' WHERE id = $1",
            &[&sid],
        )
        .await?;
        refresh_statement_totals(tx, sid).await?;
        tx.invalidate_records(
            stmt.as_records(),
            Some(&[
                "csv_import_data",
                "line_ids",
                "balance_end",
                "is_complete",
                "unreconciled_count",
            ]),
        )
        .await
    }

    /// Reconciles every open line in this statement that already has a matched invoice or
    /// counterpart account set.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_reconcile_all(
        this: &RecordSet<AccountBankStatement>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let stmt = this.ensure_one()?;
        stmt.as_records().check_access(tx, Operation::Write).await?;
        let sid = stmt.as_records().ids()[0];
        let rows = tx
            .query(
                "SELECT id::int8 FROM account_bank_statement_line \
                 WHERE statement_id = $1 AND NOT COALESCE(is_reconciled, false) \
                 ORDER BY date, id",
                &[&sid],
            )
            .await?;
        if rows.is_empty() {
            return Err(Error::user(
                "All transactions on this statement are already reconciled.",
            ));
        }
        let ids: Vec<i64> = rows
            .iter()
            .map(|r| r.try_get::<_, i64>(0).map_err(db))
            .collect::<Result<_>>()?;
        let lines = stmt
            .env()
            .records("account.bank.statement.line")?
            .browse(ids)?;
        for line in lines.iter() {
            let _: () = line.call(tx, "action_reconcile_line", ()).await?;
        }
        refresh_statement_totals(tx, sid).await?;
        tx.invalidate_records(
            stmt.as_records(),
            Some(&["balance_end", "is_complete", "unreconciled_count"]),
        )
        .await
    }

    /// Validates that the statement balances (`balance_end == balance_end_real`) and all lines
    /// are reconciled, then posts/locks the statement.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_post_statement(
        this: &RecordSet<AccountBankStatement>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let stmt = this.ensure_one()?;
        stmt.as_records().check_access(tx, Operation::Write).await?;
        let sid = stmt.as_records().ids()[0];
        refresh_statement_totals(tx, sid).await?;
        let rows = tx
            .query(
                "SELECT state::text, COALESCE(balance_end, 0), COALESCE(balance_end_real, 0), \
                        COALESCE(unreconciled_count, 0) \
                 FROM account_bank_statement WHERE id = $1",
                &[&sid],
            )
            .await?;
        let Some(row) = rows.first() else {
            return Err(Error::missing("Bank statement not found."));
        };
        let state: String = row.try_get(0).map_err(db)?;
        if state != "open" {
            return Err(Error::user("Only open bank statements can be posted."));
        }
        let end_computed: Decimal = row.try_get(1).map_err(db)?;
        let end_real: Decimal = row.try_get(2).map_err(db)?;
        let unrec: i32 = row.try_get(3).map_err(db)?;
        if end_computed != end_real {
            let diff = end_real - end_computed;
            return Err(Error::user(format!(
                "Ending balance ({end_real}) does not match computed balance ({end_computed}); \
                 difference is {diff}."
            )));
        }
        if unrec > 0 {
            return Err(Error::user(format!(
                "{unrec} statement line(s) are not yet reconciled; reconcile all transactions \
                 before posting the statement."
            )));
        }
        tx.execute(
            "UPDATE account_bank_statement SET state = 'posted', \
             write_date = now() AT TIME ZONE 'UTC' WHERE id = $1",
            &[&sid],
        )
        .await?;
        tx.invalidate_records(stmt.as_records(), Some(&["state"]))
            .await
    }

    /// Reopens a posted statement for additional adjustments.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_reopen(
        this: &RecordSet<AccountBankStatement>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let stmt = this.ensure_one()?;
        stmt.as_records().check_access(tx, Operation::Write).await?;
        let sid = stmt.as_records().ids()[0];
        tx.execute(
            "UPDATE account_bank_statement SET state = 'open', \
             write_date = now() AT TIME ZONE 'UTC' WHERE id = $1",
            &[&sid],
        )
        .await?;
        tx.invalidate_records(stmt.as_records(), Some(&["state"]))
            .await
    }
}

/// `account.bank.statement.line`: a single bank transaction that can be matched against an
/// open invoice/bill or booked to a counterpart account in one click.
#[model(
    name = "account.bank.statement.line",
    description = "Bank Statement Line",
    order = "date desc, id desc",
    check_company_auto = true,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct AccountBankStatementLine {
    /// Parent bank statement (optional for standalone bank feed transactions).
    #[field(
        many2one,
        comodel = "account.bank.statement",
        ondelete = "cascade",
        index = "btree_not_null",
        string = "Statement"
    )]
    pub statement_id: Option<RecId>,
    /// Bank, cash or credit-card journal.
    #[field(
        many2one,
        comodel = "account.journal",
        required = true,
        check_company = true,
        ondelete = "restrict",
        domain = [("type", "in", ["bank", "cash", "credit"])],
        string = "Journal"
    )]
    pub journal_id: Option<RecId>,
    /// Owning company from the journal.
    #[field(
        many2one,
        comodel = "res.company",
        related = "journal_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Currency from the journal or its company.
    #[field(
        many2one,
        comodel = "res.currency",
        compute = "_compute_currency",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Transaction date.
    #[field(date, required = true, default_fn = default_today, string = "Date")]
    pub date: Option<Date>,
    /// Bank statement description / label.
    #[field(char, required = true, string = "Label")]
    pub payment_ref: Option<Arc<str>>,
    /// Optional bank reference.
    #[field(char, string = "Reference")]
    pub r#ref: Option<Arc<str>>,
    /// Optional customer or vendor partner.
    #[field(
        many2one,
        comodel = "res.partner",
        check_company = true,
        ondelete = "restrict",
        string = "Partner"
    )]
    pub partner_id: Option<RecId>,
    /// Transaction amount: positive = inflow (deposit), negative = outflow (withdrawal).
    #[field(monetary, currency_field = "currency_id", string = "Amount")]
    pub amount: Decimal,
    /// Open invoice or vendor bill to match and settle on reconciliation.
    #[field(
        many2one,
        comodel = "account.move",
        check_company = true,
        ondelete = "set null",
        domain = [
            ("state", "=", "posted"),
            ("payment_state", "in", ["not_paid", "partial"]),
            ("move_type", "in", ["out_invoice", "out_refund", "in_invoice", "in_refund"]),
        ],
        string = "Match Invoice/Bill"
    )]
    pub matched_move_id: Option<RecId>,
    /// Counterpart account (e.g. Bank Charges, Revenue, Suspense) when not matching an invoice.
    #[field(
        many2one,
        comodel = "account.account",
        check_company = true,
        ondelete = "restrict",
        string = "Counterpart Account"
    )]
    pub counterpart_account_id: Option<RecId>,
    /// Posted journal entry created when this line was reconciled.
    #[field(
        many2one,
        comodel = "account.move",
        readonly = true,
        copy = false,
        ondelete = "set null",
        string = "Journal Entry"
    )]
    pub move_id: Option<RecId>,
    /// True once reconciled into a posted journal entry.
    #[field(
        boolean,
        readonly = true,
        copy = false,
        default = false,
        string = "Reconciled"
    )]
    pub is_reconciled: bool,
}

#[methods(AccountBankStatementLine, addon = "account")]
impl AccountBankStatementLine {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<AccountBankStatementLine>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<erp_sdk::ValueMap> {
        let mut values: erp_sdk::ValueMap =
            sup.call(tx, this.as_records(), (names.clone(),)).await?;
        let env = this.env();
        if let Some(cid) = env.company_id()?
            && names.iter().any(|n| n == "journal_id")
            && !values.contains_key("journal_id")
        {
            let jrows = tx
                .query(
                    "SELECT id::int8 FROM account_journal \
                     WHERE company_id = $1 AND type IN ('bank', 'cash') AND active = true \
                     ORDER BY CASE type WHEN 'bank' THEN 0 ELSE 1 END, sequence, id LIMIT 1",
                    &[&cid],
                )
                .await?;
            if let Some(jid) = jrows.first().and_then(|r| r.try_get::<_, i64>(0).ok()) {
                values.insert("journal_id".to_owned(), Value::Int(jid));
            }
        }
        Ok(values)
    }

    #[compute(candidate, depends = ["journal_id"])]
    async fn _compute_currency(
        this: &RecordSet<AccountBankStatementLine>,
        tx: &mut ReadTx<'_>,
        out: &mut Assign<'_, AccountBankStatementLine>,
    ) -> Result<()> {
        let journals = tx
            .mapped(this, AccountBankStatementLine::JOURNAL_ID)
            .await?;
        for (i, journal) in journals.into_iter().enumerate() {
            let mut cur = None;
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
                    let cid: Option<i64> = row.try_get(0).map_err(db)?;
                    cur = cid.and_then(RecId::from_real);
                }
            }
            out.set(i, AccountBankStatementLine::CURRENCY_ID, cur)?;
        }
        Ok(())
    }

    #[method(override)]
    async fn create(
        this: &RecordSet<AccountBankStatementLine>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let mut stmt_ids = Vec::new();
        for vals in &mut vals_list {
            if let Some(Cell::Id(sid)) = vals.get("statement_id") {
                stmt_ids.push(*sid);
                if !vals.contains("journal_id") {
                    let rows = tx
                        .query(
                            "SELECT journal_id::int8 FROM account_bank_statement WHERE id = $1",
                            &[sid],
                        )
                        .await?;
                    if let Some(jid) = rows.first().and_then(|r| r.try_get::<_, i64>(0).ok()) {
                        vals.insert("journal_id".into(), Cell::Id(jid));
                    }
                }
            }
        }
        let created = sup.call(tx, this.as_records(), (vals_list,)).await?;
        stmt_ids.sort_unstable();
        stmt_ids.dedup();
        for sid in stmt_ids {
            refresh_statement_totals(tx, sid).await?;
        }
        Ok(created)
    }

    /// Reconciles one statement line: creates & posts a balanced bank journal entry, and when
    /// `matched_move_id` is chosen, reconciles its counterpart item against the invoice/bill's
    /// open receivable/payable item so the invoice becomes Paid or Partially Paid.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_reconcile_line(
        this: &RecordSet<AccountBankStatementLine>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let line = this.ensure_one()?;
        line.as_records().check_access(tx, Operation::Write).await?;
        let lid = line.as_records().ids()[0];
        let env = line
            .env()
            .with_context_replaced(erp_sdk::Context::default());

        let rows = tx
            .query(
                "SELECT l.statement_id::int8, l.journal_id::int8, j.company_id::int8, \
                        j.default_account_id::int8, COALESCE(j.currency_id, c.currency_id)::int8, \
                        l.date, l.payment_ref::text, l.partner_id::int8, COALESCE(l.amount, 0), \
                        l.matched_move_id::int8, l.counterpart_account_id::int8, \
                        COALESCE(l.is_reconciled, false) \
                 FROM account_bank_statement_line l \
                 JOIN account_journal j ON j.id = l.journal_id \
                 JOIN res_company c ON c.id = j.company_id \
                 WHERE l.id = $1 FOR UPDATE OF l",
                &[&lid],
            )
            .await?;
        let Some(row) = rows.first() else {
            return Err(Error::missing("Bank statement line not found."));
        };
        let statement_id: Option<i64> = row.try_get(0).map_err(db)?;
        let journal_id: i64 = row.try_get(1).map_err(db)?;
        let company_id: i64 = row.try_get(2).map_err(db)?;
        let bank_account_id: Option<i64> = row.try_get(3).map_err(db)?;
        let currency_id: i64 = row.try_get(4).map_err(db)?;
        let date: Date = row.try_get(5).map_err(db)?;
        let label: String = row.try_get(6).map_err(db)?;
        let mut partner_id: Option<i64> = row.try_get(7).map_err(db)?;
        let amount: Decimal = row.try_get(8).map_err(db)?;
        let matched_move_id: Option<i64> = row.try_get(9).map_err(db)?;
        let counterpart_account_opt: Option<i64> = row.try_get(10).map_err(db)?;
        let is_reconciled: bool = row.try_get(11).map_err(db)?;

        if is_reconciled {
            return Err(Error::user(
                "This bank statement line is already reconciled.",
            ));
        }
        if amount.is_zero() {
            return Err(Error::user(
                "A bank statement line with a zero amount cannot be reconciled.",
            ));
        }
        let bank_account = bank_account_id
            .ok_or_else(|| Error::user("The bank journal has no default account configured."))?;

        let mut target_open_line_ids: Vec<i64> = Vec::new();
        let counterpart_account = if let Some(mid) = matched_move_id {
            let mrows = tx
                .query(
                    "SELECT l.id::int8, l.account_id::int8, m.partner_id::int8 \
                     FROM account_move_line l \
                     JOIN account_move m ON m.id = l.move_id \
                     LEFT JOIN account_open_item o ON o.line_id = l.id \
                     WHERE l.move_id = $1 AND l.display_type = 'payment_term' \
                       AND l.full_reconcile_id IS NULL \
                       AND COALESCE(o.amount_residual, l.balance) <> 0 \
                     ORDER BY l.id",
                    &[&mid],
                )
                .await?;
            if mrows.is_empty() {
                return Err(Error::user(
                    "The selected invoice or bill has no open receivable/payable items left to reconcile.",
                ));
            }
            let first_acc: i64 = mrows[0].try_get(1).map_err(db)?;
            if partner_id.is_none() {
                partner_id = mrows[0].try_get(2).map_err(db)?;
            }
            for mr in &mrows {
                target_open_line_ids.push(mr.try_get(0).map_err(db)?);
            }
            first_acc
        } else if let Some(acc) = counterpart_account_opt {
            acc
        } else {
            return Err(Error::user(
                "Choose either a 'Match Invoice/Bill' or a 'Counterpart Account' before reconciling.",
            ));
        };

        let (bank_bal, counterpart_bal) = compute_statement_entry_balances(amount);

        let mut mvals = RawVals::new();
        mvals.insert("move_type".into(), Cell::Str("entry".into()));
        mvals.insert("journal_id".into(), Cell::Id(journal_id));
        mvals.insert("company_id".into(), Cell::Id(company_id));
        mvals.insert("currency_id".into(), Cell::Id(currency_id));
        mvals.insert("date".into(), Cell::Date(date));
        mvals.insert("ref".into(), Cell::Str(label.as_str().into()));
        if let Some(pid) = partner_id {
            mvals.insert("partner_id".into(), Cell::Id(pid));
        }

        let unchecked_moves = crate::moves::unchecked(&env.records("account.move")?);
        let created_move = unchecked_moves.create(tx, vec![mvals]).await?;
        let move_id = created_move.ids()[0];

        let mut bank_line = RawVals::new();
        bank_line.insert("move_id".into(), Cell::Id(move_id));
        bank_line.insert("account_id".into(), Cell::Id(bank_account));
        bank_line.insert("name".into(), Cell::Str(label.as_str().into()));
        bank_line.insert("currency_id".into(), Cell::Id(currency_id));
        bank_line.insert("balance".into(), Cell::Decimal(bank_bal));
        bank_line.insert("amount_currency".into(), Cell::Decimal(bank_bal));
        if let Some(pid) = partner_id {
            bank_line.insert("partner_id".into(), Cell::Id(pid));
        }

        let mut cp_line = RawVals::new();
        cp_line.insert("move_id".into(), Cell::Id(move_id));
        cp_line.insert("account_id".into(), Cell::Id(counterpart_account));
        cp_line.insert("name".into(), Cell::Str(label.as_str().into()));
        cp_line.insert("currency_id".into(), Cell::Id(currency_id));
        cp_line.insert("balance".into(), Cell::Decimal(counterpart_bal));
        cp_line.insert("amount_currency".into(), Cell::Decimal(counterpart_bal));
        if let Some(pid) = partner_id {
            cp_line.insert("partner_id".into(), Cell::Id(pid));
        }

        let unchecked_lines = crate::moves::unchecked(&env.records("account.move.line")?);
        let created_lines = unchecked_lines.create(tx, vec![bank_line, cp_line]).await?;
        let cp_line_id = created_lines.ids()[1];

        let move_rec = env.records("account.move")?.browse([move_id])?;
        post_moves(tx, &move_rec).await?;

        if !target_open_line_ids.is_empty() {
            target_open_line_ids.push(cp_line_id);
            target_open_line_ids.sort_unstable();
            target_open_line_ids.dedup();
            let to_rec = env
                .records("account.move.line")?
                .browse(target_open_line_ids)?;
            reconcile_lines(tx, &to_rec).await?;
        }

        tx.execute(
            "UPDATE account_bank_statement_line \
             SET move_id = $2, is_reconciled = true, counterpart_account_id = $3, \
                 partner_id = COALESCE(partner_id, $4), write_date = now() AT TIME ZONE 'UTC' \
             WHERE id = $1",
            &[&lid, &move_id, &counterpart_account, &partner_id],
        )
        .await?;
        tx.invalidate_records(
            line.as_records(),
            Some(&[
                "move_id",
                "is_reconciled",
                "counterpart_account_id",
                "partner_id",
            ]),
        )
        .await?;
        if let Some(sid) = statement_id {
            refresh_statement_totals(tx, sid).await?;
        }
        Ok(())
    }
}

async fn refresh_statement_totals(tx: &mut Transaction, statement_id: i64) -> Result<()> {
    tx.execute(
        "UPDATE account_bank_statement s \
         SET balance_end = COALESCE(s.balance_start, 0) + COALESCE(t.total, 0), \
             is_complete = (COALESCE(s.balance_start, 0) + COALESCE(t.total, 0) = COALESCE(s.balance_end_real, 0)), \
             unreconciled_count = COALESCE(t.unrec, 0), \
             write_date = now() AT TIME ZONE 'UTC' \
         FROM (SELECT COALESCE(SUM(amount), 0) AS total, \
                      count(*) FILTER (WHERE NOT COALESCE(is_reconciled, false))::int4 AS unrec \
               FROM account_bank_statement_line WHERE statement_id = $1) t \
         WHERE s.id = $1",
        &[&statement_id],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{compute_statement_entry_balances, parse_bank_statement_csv};
    use erp_sdk::{Date, Decimal};
    use std::str::FromStr;

    #[test]
    fn csv_parser_parses_exact_decimals_and_skips_header() {
        let csv = "date,label,amount,ref\n\
                   2026-11-20,Customer Wire Transfer,1250.75,WIRE-01\n\
                   2026-11-21,Monthly Bank Fee,-15.50,\n";
        let rows = parse_bank_statement_csv(csv).expect("valid csv");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].date, Date::from_ymd(2026, 11, 20).unwrap());
        assert_eq!(rows[0].amount, Decimal::from_str("1250.75").unwrap());
        assert_eq!(rows[0].reference.as_deref(), Some("WIRE-01"));
        assert_eq!(rows[1].amount, Decimal::from_str("-15.50").unwrap());
        assert_eq!(rows[1].reference, None);
    }

    #[test]
    fn statement_entry_balances_sum_to_exact_zero() {
        let inflow = Decimal::from_str("500.25").unwrap();
        let (b1, c1) = compute_statement_entry_balances(inflow);
        assert_eq!(b1, inflow);
        assert_eq!(c1, -inflow);
        assert_eq!(b1 + c1, Decimal::ZERO);

        let outflow = Decimal::from_str("-42.10").unwrap();
        let (b2, c2) = compute_statement_entry_balances(outflow);
        assert_eq!(b2, outflow);
        assert_eq!(c2, -outflow);
        assert_eq!(b2 + c2, Decimal::ZERO);
    }
}
