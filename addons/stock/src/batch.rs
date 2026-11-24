// SPDX-License-Identifier: MIT OR Apache-2.0
//! Batch transfers: one document that groups several transfers of one company and operation type
//! so that a person confirms, reserves and validates them together.
//!
//! A batch is `draft` (draft transfers may join), `in_progress` (confirmed), then `done` or
//! `cancel`. Transfers join through [`add_pickings`] (the wizard `stock.picking.to.batch` and the
//! batch form call it); a transfer that does not fit is named in the refusal and nothing is added.
//!
//! Validation does not run in one transaction. `action_done` marks the batch as validating,
//! counts a new run and queues the job [`VALIDATE_KIND`]; each chunk of the job (at most
//! [`CHUNK_KEY`] transfers) validates every transfer in its own savepoint and keeps one result row
//! per transfer and run. A transfer that fails stays in the batch with its message; the others are
//! done. A transfer without any reserved quantity leaves the batch (result `detached`). A run is
//! never repeated for a transfer that already has a result in it, so a retried chunk is harmless.
//!
//! The feature group `stock.group_stock_picking_batch` shows the menu, the button and the field, and
//! every batch action checks it (an administrator is not exempt unless acting as the system).
//!
//! Not here yet: waves, automatic batching, merging batches, the batch report, scheduled date and
//! responsible propagation to the transfers, and the batch state following a transfer that is
//! validated or cancelled on its own (the state is settled at the end of a validation).

use erp_sdk::jobs::{
    EnqueueRequest, HandlerRef, JobKey, JobKindDecl, OnDuplicate, StepArgs, StepOutcome,
};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::json;
use erp_sdk::{CompanyId, DefaultContext, ErrorKind, Operation};

use crate::move_counters::{cell_id, lock_pickings};
use crate::picking::StockPicking;

/// The job kind of a batch validation.
pub const VALIDATE_KIND: &str = "stock.batch_validate";
/// Transfers one chunk of a batch validation validates (system parameter).
pub const CHUNK_KEY: &str = "stock.batch_validate_chunk";
/// The sequence code of batch references.
const SEQUENCE_CODE: &str = "stock.picking.batch";
/// Longest stored message of one transfer result (characters).
const MAX_MESSAGE: usize = 500;
/// Fields only the batch's own actions write.
const OWN_FIELDS: [&str; 3] = ["state", "validation_run", "validating"];

fn current_user(ctx: &DefaultContext) -> Value {
    Value::Int(ctx.uid)
}

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("A batch or its transfers could not be read.").with_source(error)
}

/// Set while a batch action writes the fields that only it may write.
#[derive(Default)]
pub(crate) struct OwnWrite(pub(crate) bool);

/// Set while a batch action writes `batch_id` of transfers.
#[derive(Default)]
pub(crate) struct LinkWrite(pub(crate) bool);

/// `stock.picking.batch`: transfers processed together.
#[model(
    name = "stock.picking.batch",
    description = "Batch Transfer",
    order = "priority desc, name desc",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    keys = [key(
        name = "_name_uniq",
        fields = ["company_id", "name"],
        message = "Another batch of this company already has this reference."
    )],
)]
pub struct StockPickingBatch {
    /// Reference, drawn from the batch sequence on create.
    #[field(
        char,
        required = true,
        copy = false,
        default = "/",
        string = "Batch Transfer"
    )]
    pub name: Option<Arc<str>>,
    /// Free description.
    #[field(char, string = "Description")]
    pub description: Option<Arc<str>>,
    /// Person in charge.
    #[field(
        many2one,
        comodel = "res.users",
        default_fn = current_user,
        index = "btree_not_null",
        string = "Responsible"
    )]
    pub user_id: Option<RecId>,
    /// Company; every transfer of the batch belongs to it.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict",
        index = false,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// The transfers of the batch.
    #[field(
        one2many,
        comodel = "stock.picking",
        inverse_name = "batch_id",
        string = "Transfers"
    )]
    pub picking_ids: Vec<RecId>,
    /// Operation type shared by the transfers; taken from the first transfer added when empty.
    #[field(
        many2one,
        comodel = "stock.picking.type",
        index = "btree_not_null",
        string = "Operation Type"
    )]
    pub picking_type_id: Option<RecId>,
    /// Priority.
    #[field(
        selection,
        selection = [("0", "Normal"), ("1", "Urgent")],
        default = "0",
        string = "Priority"
    )]
    pub priority: Option<Arc<str>>,
    /// Scheduled date.
    #[field(datetime, copy = false, string = "Scheduled Date")]
    pub scheduled_date: Option<DateTime>,
    /// Status.
    #[field(
        selection,
        selection = [
            ("draft", "Draft"),
            ("in_progress", "In progress"),
            ("done", "Done"),
            ("cancel", "Cancelled")
        ],
        required = true,
        readonly = true,
        copy = false,
        default = "draft",
        index = true,
        string = "Status"
    )]
    pub state: Option<Arc<str>>,
    /// Number of the last validation run; results carry it.
    #[field(integer, required = true, readonly = true, copy = false, default = 0)]
    pub validation_run: i32,
    /// A validation job is queued or running.
    #[field(
        boolean,
        readonly = true,
        copy = false,
        default = false,
        string = "Validating"
    )]
    pub validating: bool,
    /// What each validation did to each transfer.
    #[field(
        one2many,
        comodel = "stock.picking.batch.result",
        inverse_name = "batch_id",
        string = "Validation Results"
    )]
    pub result_ids: Vec<RecId>,
}

/// `stock.picking.batch.result`: the outcome of one run for one transfer.
///
/// No default records: a result exists only after a validation.
#[model(
    name = "stock.picking.batch.result",
    description = "Batch Validation Result",
    order = "run desc, id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    keys = [key(
        name = "_run_picking_uniq",
        fields = ["batch_id", "run", "picking_id"],
        message = "A transfer has one result per validation run."
    )],
)]
pub struct StockPickingBatchResult {
    /// The batch.
    #[field(
        many2one,
        comodel = "stock.picking.batch",
        required = true,
        ondelete = "cascade",
        index = false,
        string = "Batch"
    )]
    pub batch_id: Option<RecId>,
    /// The transfer.
    #[field(
        many2one,
        comodel = "stock.picking",
        required = true,
        ondelete = "cascade",
        index = "btree_not_null",
        string = "Transfer"
    )]
    pub picking_id: Option<RecId>,
    /// Company of the batch.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict",
        index = false,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// The validation run of the batch.
    #[field(integer, required = true, string = "Run")]
    pub run: i32,
    /// What happened.
    #[field(
        selection,
        selection = [
            ("done", "Done"),
            ("error", "Failed"),
            ("detached", "Removed from the batch")
        ],
        required = true,
        string = "Result"
    )]
    pub outcome: Option<Arc<str>>,
    /// The reason of a failure or removal.
    #[field(char, string = "Message")]
    pub message: Option<Arc<str>>,
}

/// `stock.picking.to.batch`: the dialog that adds transfers to a new or an existing batch.
///
/// No default records: a dialog's rows are temporary.
#[model(
    name = "stock.picking.to.batch",
    description = "Add Transfers to a Batch",
    kind = "transient",
    transient_max_age_seconds = 3600
)]
pub struct StockPickingToBatch {
    /// Add to a new batch or to an existing one.
    #[field(
        selection,
        selection = [("new", "A new batch transfer"), ("existing", "An existing batch transfer")],
        required = true,
        default = "new",
        string = "Add to"
    )]
    pub mode: Option<Arc<str>>,
    /// The existing batch.
    #[field(
        many2one,
        comodel = "stock.picking.batch",
        domain = [("state", "in", ["draft", "in_progress"])],
        string = "Batch Transfer"
    )]
    pub batch_id: Option<RecId>,
    /// Description of a new batch.
    #[field(char, string = "Description")]
    pub description: Option<Arc<str>>,
    /// Person in charge of a new batch.
    #[field(many2one, comodel = "res.users", default_fn = current_user, string = "Responsible")]
    pub user_id: Option<RecId>,
    /// Leave a new batch as a draft instead of confirming it.
    #[field(boolean, default = true, string = "Keep as draft")]
    pub is_create_draft: bool,
    /// The transfers to add.
    #[field(
        many2many,
        comodel = "stock.picking",
        relation = "stock_picking_to_batch_rel",
        column1 = "wizard_id",
        column2 = "picking_id",
        required = true,
        string = "Transfers"
    )]
    pub picking_ids: Vec<RecId>,
}

erp_sdk::jobs::register_job_kind!(
    BATCH_VALIDATE_KIND,
    JobKindDecl::new(
        "stock",
        VALIDATE_KIND,
        HandlerRef::Step {
            model: "stock.picking.batch",
            method: "run_validate_step",
        },
    )
);

/// A batch whose state follows from how many of its transfers are open and done.
///
/// `None` while a transfer is still open; a batch that lost every transfer is cancelled.
fn settled_state(total: i64, open: i64, done: i64) -> Option<&'static str> {
    if open > 0 {
        None
    } else if total == 0 || done == 0 {
        Some("cancel")
    } else {
        Some("done")
    }
}

/// The feature group of batch transfers.
const GROUP: &str = "stock.group_stock_picking_batch";

/// The batch actions are for users of the feature group.
async fn require_group(env: &Env, tx: &mut Transaction) -> Result<()> {
    if env.su() || tx.has_group(env, GROUP).await? {
        Ok(())
    } else {
        Err(Error::access(
            "Batch transfers are not turned on for you: ask an administrator to enable Batch transfers in the Inventory settings.",
        ))
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_MESSAGE).collect()
}

/// True when `vals` names `field`, as a plain value or as an x2many command list.
fn names(vals: &RawVals, field: &str) -> bool {
    vals.contains(field) || vals.iter_commands().any(|(name, _)| name == field)
}

fn guard_own_fields(vals: &RawVals) -> Result<()> {
    match OWN_FIELDS.iter().find(|field| vals.contains(field)) {
        Some(field) => Err(Error::validation(format!(
            "The field {field} of a batch is set by its actions (Confirm, Validate, Cancel), not written directly."
        ))),
        None => Ok(()),
    }
}

/// Refuse a write of `batch_id` on a transfer unless a batch action is running.
///
/// # Errors
/// `Validation` outside a batch action.
pub(crate) fn refuse_link(tx: &mut Transaction) -> Result<()> {
    if tx.extension::<LinkWrite>().0 {
        Ok(())
    } else {
        Err(Error::validation(
            "A transfer joins or leaves a batch only through the batch actions (Add to Batch, Cancel, Validate).",
        ))
    }
}

/// Write the fields only a batch action sets.
async fn write_own(records: &Records, tx: &mut Transaction, vals: RawVals) -> Result<()> {
    let previous = std::mem::replace(&mut tx.extension::<OwnWrite>().0, true);
    let written = records.write(tx, vals).await;
    tx.extension::<OwnWrite>().0 = previous;
    written
}

/// Join (`Some`) or leave (`None`) a batch.
async fn link(
    env: &Env,
    tx: &mut Transaction,
    pickings: Vec<i64>,
    batch: Option<i64>,
) -> Result<()> {
    let value = batch.map_or(Cell::Null, Cell::Id);
    let records = env.records("stock.picking")?.browse(pickings)?;
    let previous = std::mem::replace(&mut tx.extension::<LinkWrite>().0, true);
    let written = records
        .write(tx, RawVals::new().set_cell("batch_id", value))
        .await;
    tx.extension::<LinkWrite>().0 = previous;
    written
}

struct Header {
    name: String,
    state: String,
    company: i64,
    picking_type: Option<i64>,
    validating: bool,
    run: i32,
}

async fn header(tx: &mut Transaction, id: i64) -> Result<Header> {
    header_with(tx, id, "FOR UPDATE").await
}

/// The header with an optional lock clause (empty: read without locking).
async fn header_with(tx: &mut Transaction, id: i64, lock: &str) -> Result<Header> {
    tx.flush().await?;
    let rows = tx
        .query(
            &format!(
                "SELECT name, state, company_id, picking_type_id, validating, validation_run \
                 FROM stock_picking_batch WHERE id = $1 {lock}"
            ),
            &[&id],
        )
        .await?;
    let [row] = rows.as_slice() else {
        return Err(Error::user("This batch no longer exists."));
    };
    Ok(Header {
        name: row.try_get(0).map_err(decode)?,
        state: row.try_get(1).map_err(decode)?,
        company: row.try_get(2).map_err(decode)?,
        picking_type: row.try_get(3).map_err(decode)?,
        validating: row.try_get(4).map_err(decode)?,
        run: row.try_get(5).map_err(decode)?,
    })
}

fn job_key(id: i64, run: i32) -> String {
    format!("{VALIDATE_KIND}:{id}:{run}")
}

/// A validation counts as running only while a live job holds its key: a job that died, was
/// cancelled or discarded leaves the flag set, and the batch is then free again.
async fn held(tx: &mut Transaction, id: i64, head: &Header) -> Result<bool> {
    if !head.validating {
        return Ok(false);
    }
    let key = job_key(id, head.run);
    Ok(!tx
        .query(
            "SELECT 1 FROM erp_job WHERE kind = $1 AND dedup_key = $2 AND state <> 'dead'",
            &[&VALIDATE_KIND, &key],
        )
        .await?
        .is_empty())
}

/// Check the caller may change the batch (and the transfers `ids`) before anything is read raw.
async fn check_write(env: &Env, tx: &mut Transaction, batch: i64, ids: &[i64]) -> Result<()> {
    env.records("stock.picking.batch")?
        .browse([batch])?
        .check_access(tx, Operation::Write)
        .await?;
    if !ids.is_empty() {
        env.records("stock.picking")?
            .browse(ids.to_vec())?
            .check_access(tx, Operation::Write)
            .await?;
    }
    Ok(())
}

/// Add `ids` to batch `batch`, or refuse naming every transfer that does not fit.
///
/// A transfer fits when it is Waiting, Waiting for stock or Ready (or Draft, for a draft batch),
/// has the batch's company and operation type and is in no other batch. A batch without an
/// operation type takes the one of its first transfer. Nothing joins a batch that is being
/// validated.
///
/// # Errors
/// `User` for a batch that is done, cancelled or validating, a missing transfer or an
/// incompatible one; the caller's access errors.
pub(crate) async fn add_pickings(
    env: &Env,
    tx: &mut Transaction,
    batch: i64,
    ids: &[i64],
) -> Result<()> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Err(Error::user("Select at least one transfer to add."));
    }
    require_group(env, tx).await?;
    check_write(env, tx, batch, &ids).await?;
    let head = header(tx, batch).await?;
    if !matches!(head.state.as_str(), "draft" | "in_progress") {
        return Err(Error::user(format!(
            "Batch {} is {}: transfers can only be added to a draft or in-progress batch.",
            head.name,
            if head.state == "done" {
                "done"
            } else {
                "cancelled"
            }
        )));
    }
    if held(tx, batch, &head).await? {
        return Err(Error::user(format!(
            "Batch {} is being validated: add transfers when it is finished.",
            head.name
        )));
    }
    lock_pickings(tx, &ids).await?;
    let rows = tx
        .query(
            "SELECT id, name, state, company_id, picking_type_id, batch_id \
             FROM stock_picking WHERE id = ANY($1) ORDER BY id",
            &[&ids],
        )
        .await?;
    if rows.len() != ids.len() {
        return Err(Error::user(
            "Some of the selected transfers no longer exist.",
        ));
    }
    let mut candidates = Vec::new();
    for row in &rows {
        candidates.push((
            row.try_get::<_, String>(1).map_err(decode)?,
            row.try_get::<_, String>(2).map_err(decode)?,
            row.try_get::<_, i64>(3).map_err(decode)?,
            row.try_get::<_, i64>(4).map_err(decode)?,
            row.try_get::<_, Option<i64>>(5).map_err(decode)?,
        ));
    }
    let picking_type = head.picking_type.unwrap_or(candidates[0].3);
    let misfits: Vec<&str> = candidates
        .iter()
        .filter(|(_, state, company, kind, other)| {
            let state_ok = matches!(state.as_str(), "waiting" | "confirmed" | "assigned")
                || (state == "draft" && head.state == "draft");
            !state_ok
                || *company != head.company
                || *kind != picking_type
                || other.is_some_and(|other| other != batch)
        })
        .map(|(name, ..)| name.as_str())
        .collect();
    if !misfits.is_empty() {
        return Err(Error::user(format!(
            "These transfers cannot join batch {}: {}. A transfer must be Waiting, Ready or (for a draft batch) \
             Draft, belong to the batch's company and operation type, and not be in another batch.",
            head.name,
            misfits.join(", ")
        )));
    }
    // Written in every case: the update makes a concurrent type or company write that waited on
    // the batch row fail with a serialization error and retry, instead of passing on an old snapshot.
    let records = env.records("stock.picking.batch")?.browse([batch])?;
    write_own(
        &records,
        tx,
        RawVals::new().set_cell("picking_type_id", Cell::Id(picking_type)),
    )
    .await?;
    link(env, tx, ids, Some(batch)).await
}

/// Open transfers of the batch that have no result in `run`.
const OPEN_FILTER: &str = "p.batch_id = $1 AND p.state NOT IN ('done', 'cancel') \
     AND NOT EXISTS (SELECT 1 FROM stock_picking_batch_result r \
          WHERE r.batch_id = $1 AND r.run = $2 AND r.picking_id = p.id)";
/// A transfer has a reserved quantity on an open move.
const HAS_QUANTITY: &str = "EXISTS (SELECT 1 FROM stock_move m JOIN stock_move_line l ON l.move_id = m.id \
     WHERE m.picking_id = p.id AND m.state NOT IN ('done', 'cancel') AND l.quantity > 0)";

async fn count_one(tx: &mut Transaction, sql: &str, batch: i64, run: i32) -> Result<i64> {
    tx.flush().await?;
    tx.query(sql, &[&batch, &run])
        .await?
        .first()
        .ok_or_else(|| Error::internal("A count returned no row."))?
        .try_get(0)
        .map_err(decode)
}

async fn count_open(tx: &mut Transaction, batch: i64, run: i32) -> Result<i64> {
    count_one(
        tx,
        &format!("SELECT count(*) FROM stock_picking p WHERE {OPEN_FILTER}"),
        batch,
        run,
    )
    .await
}

/// Set the batch's end state when no transfer is open any more, and clear its validating flag.
async fn settle(env: &Env, tx: &mut Transaction, id: i64) -> Result<()> {
    tx.flush().await?;
    let counts = tx
        .query(
            "SELECT count(*), count(*) FILTER (WHERE state NOT IN ('done', 'cancel')), \
                    count(*) FILTER (WHERE state = 'done') \
             FROM stock_picking WHERE batch_id = $1",
            &[&id],
        )
        .await?;
    let [row] = counts.as_slice() else {
        return Err(Error::internal("A count returned no row."));
    };
    let mut vals = RawVals::new().set_cell("validating", Cell::Bool(false));
    if let Some(state) = settled_state(
        row.try_get(0).map_err(decode)?,
        row.try_get(1).map_err(decode)?,
        row.try_get(2).map_err(decode)?,
    ) {
        vals = vals.set_cell("state", Cell::Str(state.into()));
    }
    write_own(&env.records("stock.picking.batch")?.browse([id])?, tx, vals).await
}

/// The errors a person can act on are kept as a transfer's result; any other error (a lock
/// timeout, a database or programming error) is returned so the queue retries or dead-letters.
fn is_user_facing(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::UserError
            | ErrorKind::Validation
            | ErrorKind::Conflict
            | ErrorKind::Access
            | ErrorKind::AccessDenied
            | ErrorKind::Missing
    )
}

#[methods(StockPickingBatch, addon = "stock")]
impl StockPickingBatch {
    /// A new batch takes its company and reference; its status fields and transfers are the
    /// actions' own.
    #[method(override)]
    async fn create(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        let records = this.as_records();
        require_group(records.env(), tx).await?;
        records.check_access(tx, Operation::Create).await?;
        let env = records.env().clone();
        for vals in &mut vals_list {
            guard_own_fields(vals)?;
            if names(vals, "picking_ids") {
                return Err(Error::validation(
                    "Add transfers to a batch with Add Transfers or the Add to Batch button of a transfer.",
                ));
            }
            if !vals.contains("company_id") {
                let company = env.company_id()?.ok_or_else(|| {
                    Error::user("Select a company before creating a batch transfer.")
                })?;
                vals.insert("company_id".into(), Cell::Id(company));
            }
            let named = matches!(vals.get("name"), Some(Cell::Str(name)) if !name.is_empty() && &**name != "/");
            if !named {
                let company = vals
                    .get("company_id")
                    .and_then(cell_id)
                    .ok_or_else(|| Error::user("A batch transfer needs a company."))?;
                let name = erp_sdk::sequence::draw_by_code(
                    tx,
                    &env,
                    SEQUENCE_CODE,
                    CompanyId::new(company)?,
                    None,
                )
                .await?;
                vals.insert("name".into(), Cell::Str(name.into()));
            }
        }
        sup.call(tx, records, (vals_list,)).await
    }

    /// Status fields and the transfer list are changed by the actions only, and the operation type
    /// and company of a batch that holds transfers do not change.
    #[method(override)]
    async fn write(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        if !tx.extension::<OwnWrite>().0 {
            require_group(this.as_records().env(), tx).await?;
            guard_own_fields(&vals)?;
            if names(&vals, "picking_ids") {
                return Err(Error::validation(
                    "Add transfers to a batch with Add Transfers or the Add to Batch button of a transfer.",
                ));
            }
            let records = this.as_records();
            if !records.is_empty()
                && ["picking_type_id", "company_id"]
                    .iter()
                    .any(|f| vals.contains(f))
            {
                records.check_access(tx, Operation::Write).await?;
                tx.flush().await?;
                // the batch rows are locked first: a concurrent add waits for this write
                tx.query(
                    "SELECT 1 FROM stock_picking_batch WHERE id = ANY($1) ORDER BY id FOR UPDATE",
                    &[&records.ids()],
                )
                .await?;
                let used = tx
                    .query(
                        "SELECT 1 FROM stock_picking WHERE batch_id = ANY($1) LIMIT 1",
                        &[&records.ids()],
                    )
                    .await?;
                if !used.is_empty() {
                    return Err(Error::validation(
                        "The operation type and company of a batch that holds transfers cannot change.",
                    ));
                }
            }
        }
        sup.call(tx, this.as_records(), (vals,)).await
    }

    /// A batch is deleted only while it is not being validated and none of its transfers is done:
    /// it is the record of what was processed.
    #[method(override)]
    async fn unlink(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        let records = this.as_records();
        if records.is_empty() {
            return sup.call(tx, records, ()).await;
        }
        require_group(records.env(), tx).await?;
        records.check_access(tx, Operation::Unlink).await?;
        tx.flush().await?;
        let ids = records.ids();
        for id in &ids {
            let head = header(tx, *id).await?;
            if held(tx, *id, &head).await? {
                return Err(Error::user(format!(
                    "Batch {} is being validated and cannot be deleted.",
                    head.name
                )));
            }
        }
        let kept = tx
            .query(
                "SELECT 1 FROM stock_picking_batch b WHERE b.id = ANY($1) AND (b.state = 'done' \
                   OR EXISTS (SELECT 1 FROM stock_picking p WHERE p.batch_id = b.id AND p.state = 'done')) \
                 LIMIT 1",
                &[&ids],
            )
            .await?;
        if !kept.is_empty() {
            return Err(Error::user(
                "A batch with a done transfer is the record of what was processed and cannot be deleted.",
            ));
        }
        sup.call(tx, records, ()).await
    }

    /// Confirm the draft transfers and start the batch.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_confirm(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        let id = records.ensure_one()?.int()?;
        require_group(records.env(), tx).await?;
        records.check_access(tx, Operation::Write).await?;
        let head = header(tx, id).await?;
        if head.state != "draft" {
            return Err(Error::user(format!(
                "Batch {} is not a draft: only a draft batch is confirmed.",
                head.name
            )));
        }
        let rows = tx
            .query(
                "SELECT id, state FROM stock_picking WHERE batch_id = $1 ORDER BY id",
                &[&id],
            )
            .await?;
        if rows.is_empty() {
            return Err(Error::user(
                "Add transfers to the batch before confirming it.",
            ));
        }
        let mut drafts = Vec::new();
        for row in &rows {
            if row.try_get::<_, String>(1).map_err(decode)? == "draft" {
                drafts.push(row.try_get::<_, i64>(0).map_err(decode)?);
            }
        }
        if !drafts.is_empty() {
            let pickings = records.env().records("stock.picking")?.browse(drafts)?;
            pickings.call::<(), ()>(tx, "action_confirm", ()).await?;
        }
        write_own(
            records,
            tx,
            RawVals::new().set_cell("state", Cell::Str("in_progress".into())),
        )
        .await
    }

    /// Check availability of every transfer of the batch.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_assign(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        let id = records.ensure_one()?.int()?;
        require_group(records.env(), tx).await?;
        records.check_access(tx, Operation::Write).await?;
        let head = header(tx, id).await?;
        if head.state != "in_progress" || held(tx, id, &head).await? {
            return Err(Error::user(format!(
                "Batch {} must be in progress and not being validated to check availability.",
                head.name
            )));
        }
        let ids: Vec<i64> = tx
            .query(
                "SELECT id FROM stock_picking WHERE batch_id = $1 \
                   AND state IN ('confirmed', 'waiting', 'assigned') ORDER BY id",
                &[&id],
            )
            .await?
            .iter()
            .map(|row| row.try_get(0))
            .collect::<std::result::Result<_, _>>()
            .map_err(decode)?;
        if ids.is_empty() {
            return Err(Error::user("No transfer of this batch can be reserved."));
        }
        records
            .env()
            .records("stock.picking")?
            .browse(ids)?
            .call::<(), ()>(tx, "action_assign", ())
            .await
    }

    /// Cancel the batch: its open transfers leave it unchanged; done ones stay as its record.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_cancel(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        let id = records.ensure_one()?.int()?;
        require_group(records.env(), tx).await?;
        records.check_access(tx, Operation::Write).await?;
        let head = header(tx, id).await?;
        let running = held(tx, id, &head).await?;
        if !matches!(head.state.as_str(), "draft" | "in_progress") || running {
            return Err(Error::user(format!(
                "Batch {} cannot be cancelled while it is {}.",
                head.name,
                if running {
                    "being validated"
                } else {
                    "done or cancelled"
                }
            )));
        }
        let open: Vec<i64> = tx
            .query(
                "SELECT id FROM stock_picking WHERE batch_id = $1 AND state <> 'done' ORDER BY id",
                &[&id],
            )
            .await?
            .iter()
            .map(|row| row.try_get(0))
            .collect::<std::result::Result<_, _>>()
            .map_err(decode)?;
        if !open.is_empty() {
            link(records.env(), tx, open, None).await?;
        }
        write_own(
            records,
            tx,
            RawVals::new()
                .set_cell("state", Cell::Str("cancel".into()))
                .set_cell("validating", Cell::Bool(false)),
        )
        .await
    }

    /// Validate the batch: queue the chunked job that validates its transfers one by one. A
    /// batch whose transfers are all done or cancelled is settled at once. A validation whose job
    /// is gone is queued again, continuing the same run.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_done(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records();
        let id = records.ensure_one()?.int()?;
        require_group(records.env(), tx).await?;
        records.check_access(tx, Operation::Write).await?;
        let head = header(tx, id).await?;
        if head.state != "in_progress" {
            return Err(Error::user(format!(
                "Batch {} is not in progress: confirm it before validating.",
                head.name
            )));
        }
        if held(tx, id, &head).await? {
            return Err(Error::user(format!(
                "Batch {} is already being validated.",
                head.name
            )));
        }
        let run = if head.validating {
            head.run
        } else {
            head.run + 1
        };
        if count_open(tx, id, run).await? == 0 {
            return settle(records.env(), tx, id).await;
        }
        let with_quantity = count_one(
            tx,
            &format!("SELECT count(*) FROM stock_picking p WHERE {OPEN_FILTER} AND {HAS_QUANTITY}"),
            id,
            run,
        )
        .await?;
        if with_quantity == 0 {
            return Err(Error::user(format!(
                "No open transfer of batch {} has a reserved quantity: use Check Availability first.",
                head.name
            )));
        }
        write_own(
            records,
            tx,
            RawVals::new()
                .set_cell("validation_run", Cell::Int(run))
                .set_cell("validating", Cell::Bool(true)),
        )
        .await?;
        let env = tx.env();
        tx.enqueue(
            &env,
            EnqueueRequest::new(VALIDATE_KIND)
                .payload(&json!({ "id": id, "run": run }))?
                .dedup(JobKey::new(&job_key(id, run))?, OnDuplicate::Keep),
        )
        .await?;
        Ok(())
    }

    /// Open the dialog that adds transfers to this batch.
    #[method]
    #[rpc(effect = read)]
    pub async fn action_add_transfers(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = this.as_records().ensure_one()?.int()?;
        require_group(this.as_records().env(), tx).await?;
        let head = header_with(tx, id, "").await?;
        if held(tx, id, &head).await? {
            return Err(Error::user(format!(
                "Batch {} is being validated: add transfers when it is finished.",
                head.name
            )));
        }
        Ok(dialog(
            json!({ "default_mode": "existing", "default_batch_id": id }),
        ))
    }

    /// One chunk of a batch validation (job kind [`VALIDATE_KIND`]), as the user who asked for it.
    ///
    /// # Errors
    /// `Invariant` for a payload that is not `{id, run}`; an error of a transfer that is not one a
    /// person can act on (the queue retries the chunk). A transfer that cannot be validated for a
    /// reason a person can act on is recorded, not raised.
    #[method]
    async fn run_validate_step(
        this: &RecordSet<StockPickingBatch>,
        tx: &mut Transaction,
        args: StepArgs,
    ) -> Result<StepOutcome> {
        let (Some(id), Some(run)) = (
            args.payload["id"].as_i64(),
            args.payload["run"]
                .as_i64()
                .and_then(|run| i32::try_from(run).ok()),
        ) else {
            return Err(Error::invariant(
                "the batch validation payload is not {id, run}",
            ));
        };
        let env = this.as_records().env().clone();
        tx.flush().await?;
        let exists = !tx
            .query("SELECT 1 FROM stock_picking_batch WHERE id = $1", &[&id])
            .await?
            .is_empty();
        if !exists {
            return Ok(StepOutcome::done(json!({ "outcome": "gone" }), 0));
        }
        let head = header(tx, id).await?;
        if head.run != run || !head.validating {
            return Ok(StepOutcome::done(json!({ "outcome": "stale" }), 0));
        }
        let chunk = erp_sdk::params::get_int(tx, &crate::ADDON, CHUNK_KEY).await?;
        let todo = tx
            .query(
                &format!(
                    "SELECT p.id, {HAS_QUANTITY} FROM stock_picking p WHERE {OPEN_FILTER} \
                     ORDER BY p.id LIMIT $3"
                ),
                &[&id, &run, &chunk],
            )
            .await?;
        let pickings = env.records("stock.picking")?;
        let mut results = Vec::new();
        for row in &todo {
            let picking: i64 = row.try_get(0).map_err(decode)?;
            let has_quantity: bool = row.try_get(1).map_err(decode)?;
            let record = pickings.browse([picking])?;
            let (outcome, message) = if !has_quantity {
                let env = env.clone();
                let detached = tx
                    .savepoint(async move |tx| link(&env, tx, vec![picking], None).await)
                    .await;
                match detached {
                    Ok(()) => (
                        "detached",
                        "No quantity is reserved: the transfer left the batch.".to_owned(),
                    ),
                    Err(error) if is_user_facing(error.kind()) => {
                        ("error", bounded(error.message()))
                    }
                    Err(error) => return Err(error),
                }
            } else {
                let validated = tx
                    .savepoint(async move |tx| {
                        record.call::<(), ()>(tx, "button_validate", ()).await
                    })
                    .await;
                match validated {
                    Ok(()) => {
                        tx.flush().await?;
                        let state: String = tx
                            .query("SELECT state FROM stock_picking WHERE id = $1", &[&picking])
                            .await?
                            .first()
                            .ok_or_else(|| Error::internal("A transfer vanished."))?
                            .try_get(0)
                            .map_err(decode)?;
                        if state == "done" {
                            ("done", String::new())
                        } else {
                            ("error", format!("The transfer is {state}, not done."))
                        }
                    }
                    Err(error) if is_user_facing(error.kind()) => {
                        ("error", bounded(error.message()))
                    }
                    Err(error) => return Err(error),
                }
            };
            results.push(
                RawVals::new()
                    .set_cell("batch_id", Cell::Id(id))
                    .set_cell("picking_id", Cell::Id(picking))
                    .set_cell("company_id", Cell::Id(head.company))
                    .set_cell("run", Cell::Int(run))
                    .set_cell("outcome", Cell::Str(outcome.into()))
                    .set_cell(
                        "message",
                        if message.is_empty() {
                            Cell::Null
                        } else {
                            Cell::Str(message.into())
                        },
                    ),
            );
        }
        let processed = u64::try_from(results.len()).unwrap_or(0);
        if !results.is_empty() {
            // users may read results but never write them: the job writes them
            env.sudo(true)
                .records("stock.picking.batch.result")?
                .create(tx, results)
                .await?;
        }
        let left = count_open(tx, id, run).await?;
        if left > 0 {
            return Ok(StepOutcome::more(
                json!({ "id": id, "run": run }),
                processed,
                u64::try_from(left).ok(),
            ));
        }
        settle(&env, tx, id).await?;
        Ok(StepOutcome::done(
            json!({ "id": id, "run": run }),
            processed,
        ))
    }
}

fn dialog(context: erp_sdk::serde_json::Value) -> Value {
    Value::Json(Arc::new(json!({
        "type": "ir.actions.act_window",
        "name": "Add to Batch",
        "res_model": "stock.picking.to.batch",
        "target": "new",
        "views": [[false, "form"]],
        "context": context,
    })))
}

#[methods(StockPickingToBatch, addon = "stock")]
impl StockPickingToBatch {
    /// Add the chosen transfers to the chosen batch, or to a new one (confirmed unless kept as a
    /// draft), and open the batch.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_add_to_batch(
        this: &RecordSet<StockPickingToBatch>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        let id = records.ensure_one()?.int()?;
        require_group(records.env(), tx).await?;
        records.check_access(tx, Operation::Read).await?;
        tx.flush().await?;
        let ids: Vec<i64> = tx
            .query(
                "SELECT picking_id FROM stock_picking_to_batch_rel WHERE wizard_id = $1 ORDER BY picking_id",
                &[&id],
            )
            .await?
            .iter()
            .map(|row| row.try_get(0))
            .collect::<std::result::Result<_, _>>()
            .map_err(decode)?;
        if ids.is_empty() {
            return Err(Error::user("Select at least one transfer to add."));
        }
        let env = records.env().clone();
        // the caller may change the chosen transfers before anything else of them is read
        env.records("stock.picking")?
            .browse(ids.clone())?
            .check_access(tx, Operation::Write)
            .await?;
        let rows = tx
            .query(
                "SELECT mode, batch_id, is_create_draft FROM stock_picking_to_batch WHERE id = $1",
                &[&id],
            )
            .await?;
        let [dialog] = rows.as_slice() else {
            return Err(Error::user("This dialog has expired; open it again."));
        };
        let mode: String = dialog.try_get(0).map_err(decode)?;
        let chosen: Option<i64> = dialog.try_get(1).map_err(decode)?;
        let draft: bool = dialog.try_get(2).map_err(decode)?;
        let batch = if mode == "existing" {
            chosen.ok_or_else(|| Error::user("Choose the batch to add the transfers to."))?
        } else {
            let companies: Vec<i64> = tx
                .query(
                    "SELECT DISTINCT company_id FROM stock_picking WHERE id = ANY($1)",
                    &[&ids],
                )
                .await?
                .iter()
                .map(|row| row.try_get(0))
                .collect::<std::result::Result<_, _>>()
                .map_err(decode)?;
            let [company] = companies.as_slice() else {
                return Err(Error::user(
                    "The selected transfers must belong to a single company.",
                ));
            };
            let mut vals = RawVals::new().set_cell("company_id", Cell::Id(*company));
            for field in ["description", "user_id"] {
                vals = vals.set_cell(field, records.cell(tx, field).await?);
            }
            env.records("stock.picking.batch")?
                .create(tx, vec![vals])
                .await?
                .ids()[0]
        };
        add_pickings(&env, tx, batch, &ids).await?;
        if mode == "new" && !draft {
            env.records("stock.picking.batch")?
                .browse([batch])?
                .call::<(), ()>(tx, "action_confirm", ())
                .await?;
        }
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.act_window",
            "name": "Batch Transfer",
            "res_model": "stock.picking.batch",
            "res_id": batch,
            "target": "current",
            "views": [[false, "form"]],
        }))))
    }
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    /// The Add to Batch button of a transfer: opens the dialog with this transfer chosen.
    #[method]
    #[rpc(effect = read)]
    pub async fn action_add_to_batch(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        require_group(this.as_records().env(), tx).await?;
        let ids = this.as_records().ids();
        if ids.is_empty() {
            return Err(Error::user(
                "Select at least one transfer to add to a batch.",
            ));
        }
        Ok(dialog(json!({ "default_picking_ids": [[6, 0, ids]] })))
    }
}

#[cfg(test)]
mod tests {
    use super::{is_user_facing, settled_state};
    use erp_sdk::ErrorKind;

    #[test]
    fn a_batch_settles_only_when_no_transfer_is_open() {
        assert_eq!(settled_state(3, 1, 2), None);
        assert_eq!(settled_state(3, 0, 2), Some("done"));
        assert_eq!(settled_state(2, 0, 0), Some("cancel"));
        assert_eq!(settled_state(0, 0, 0), Some("cancel"));
    }

    /// Only errors a person can act on become a transfer's result; the others are returned so the
    /// queue retries the chunk.
    #[test]
    fn only_user_facing_errors_are_recorded() {
        for kind in [
            ErrorKind::UserError,
            ErrorKind::Validation,
            ErrorKind::Access,
            ErrorKind::Missing,
            ErrorKind::Conflict,
        ] {
            assert!(is_user_facing(kind), "{kind:?}");
        }
        for kind in [
            ErrorKind::Concurrency,
            ErrorKind::Lock,
            ErrorKind::Database,
            ErrorKind::Programming,
            ErrorKind::Internal,
        ] {
            assert!(!is_user_facing(kind), "{kind:?}");
        }
    }
}
