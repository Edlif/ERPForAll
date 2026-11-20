// SPDX-License-Identifier: MIT OR Apache-2.0
//! Valuation storage declarations for. Database guards are in `history_guards`.
//! Bounded numeric checks follow the valuation codec. Ordinary users have no valuation grants.

use crate::recorded_now;
use erp_sdk::prelude::*;

/// Valuation Journal: and the approved valuation table note.
#[model(
    name = "stock.valuation.journal",
    table = "stk_value_journal",
    description = "Valuation Journal",
    rec_name = "product_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    log_access = false,
    constraints = [
        ("_valid_grid_delta", r#"CHECK (CASE WHEN "grid_delta" IS NOT NULL AND "grid_delta" NOT IN ('NaN'::numeric, 'Infinity'::numeric, '-Infinity'::numeric) AND min_scale("grid_delta") <= 18 AND abs("grid_delta") < power(2::numeric, 256) THEN abs("grid_delta" * power(10::numeric, 18)) < power(2::numeric, 256) ELSE FALSE END)"#, "grid_delta must fit the 18-place valuation grid."),
        ("_id_company", "UNIQUE (id, company_id)", "A journal contribution is unique with its company."),
        ("_source_company_product", "FOREIGN KEY (source_id, company_id, product_id) REFERENCES stk_value_source (id, company_id, product_id) ON DELETE RESTRICT", "A valuation row must have its source company and product."),
        ("_lot_product", "FOREIGN KEY (lot_id, product_id) REFERENCES stock_lot (id, product_id) ON DELETE RESTRICT", "A valuation lot must belong to its product."),
        ("_finite_quantity_delta", "CHECK (quantity_delta <> 'NaN' AND quantity_delta <> 'Infinity' AND quantity_delta <> '-Infinity')", "quantity_delta must be a finite number."),
        ("_finite_grid_delta", "CHECK (grid_delta <> 'NaN' AND grid_delta <> 'Infinity' AND grid_delta <> '-Infinity')", "grid_delta must be a finite number."),
        ("_finite_booked_delta", "CHECK (booked_delta <> 'NaN' AND booked_delta <> 'Infinity' AND booked_delta <> '-Infinity')", "booked_delta must be a finite number."),
        ("_finite_currency_increment", "CHECK (currency_increment <> 'NaN' AND currency_increment <> 'Infinity' AND currency_increment <> '-Infinity')", "currency_increment must be a finite number."),
        ("_increment_positive", "CHECK (currency_increment > 0)", "The frozen currency increment must be positive."),
        ("_seq_nonneg", "CHECK (seq >= 0)", "A valuation journal sequence cannot be negative."),
        ("_role_seq", "CHECK ((role = 'initial') = (seq = 0))", "Only initial contributions have sequence zero."),
        ("_adjustment_no_quantity", "CHECK (role = 'initial' OR quantity_delta = 0)", "An adjustment cannot count physical quantity again."),
        ("_adjustment_has_cause", "CHECK (role <> 'adjustment' OR cause_journal_id IS NOT NULL)", "A valuation adjustment must name its original contribution."),
    ],
    indexes = [
        ("_cost_key_effective", "(company_id, product_id, lot_id, effective_time, source_id, id)"),
        ("_recognition", "(company_id, recognition_date, id)"),
    ],
    unique_indexes = [
        ("_identity", "(source_id, lot_id, seq) NULLS NOT DISTINCT", "A valuation journal contribution must be unique."),
    ],
)]
#[non_exhaustive]
pub struct ValuationJournal {
    /// Source id.
    #[field(
        many2one,
        comodel = "stock.valuation.source",
        ondelete = "restrict",
        required = true
    )]
    pub source_id: Option<RecId>,
    /// Company id.
    #[field(
        many2one,
        comodel = "res.company",
        ondelete = "restrict",
        required = true
    )]
    pub company_id: Option<RecId>,
    /// Product id.
    #[field(
        many2one,
        comodel = "product.product",
        ondelete = "restrict",
        required = true,
        check_company = true
    )]
    pub product_id: Option<RecId>,
    /// Lot id.
    #[field(many2one, comodel = "stock.lot", ondelete = "restrict")]
    pub lot_id: Option<RecId>,
    /// Seq.
    #[field(integer, required = true)]
    pub seq: i32,
    /// Role.
    #[field(selection, selection = [("initial", "Initial"), ("adjustment", "Adjustment"), ("mapping_transfer", "Mapping Transfer")], required = true)]
    pub role: Option<Arc<str>>,
    /// Exact quantity delta.
    #[field(float, digits = 0, required = true)]
    pub quantity_delta: Decimal,
    /// Exact grid delta.
    #[field(float, digits = 0, required = true)]
    pub grid_delta: Decimal,
    /// Exact booked delta.
    #[field(float, digits = 0, required = true)]
    pub booked_delta: Decimal,
    /// Currency id.
    #[field(
        many2one,
        comodel = "res.currency",
        ondelete = "restrict",
        required = true
    )]
    pub currency_id: Option<RecId>,
    /// Exact currency increment.
    #[field(float, digits = 0, required = true)]
    pub currency_increment: Decimal,
    /// Rounding mode.
    #[field(selection, selection = [("half_away_from_zero", "Half Away From Zero")], required = true)]
    pub rounding_mode: Option<Arc<str>>,
    /// Method.
    #[field(selection, selection = [("standard", "Standard"), ("average", "Average Cost"), ("fifo", "FIFO")], required = true)]
    pub method: Option<Arc<str>>,
    /// Policy.
    #[field(selection, selection = [("cost_scale_v1", "Cost Scale v1")], required = true)]
    pub policy: Option<Arc<str>>,
    /// Category id.
    #[field(
        many2one,
        comodel = "product.category",
        ondelete = "restrict",
        required = true
    )]
    pub category_id: Option<RecId>,
    /// Effective time (naive UTC).
    #[field(datetime, required = true)]
    pub effective_time: Option<DateTime>,
    /// Actual accounting recognition date, not the event date.
    #[field(date, required = true)]
    pub recognition_date: Option<Date>,
    /// Cause journal id.
    #[field(
        many2one,
        comodel = "stock.valuation.journal",
        ondelete = "restrict",
        index = "btree_not_null"
    )]
    pub cause_journal_id: Option<RecId>,
    /// Cause source id.
    #[field(many2one, comodel = "stock.valuation.source", ondelete = "restrict")]
    pub cause_source_id: Option<RecId>,
    /// Generation.
    #[field(integer, required = true)]
    pub generation: i32,
    /// Recorded at (naive UTC).
    #[field(datetime, required = true, default_fn = recorded_now)]
    pub recorded_at: Option<DateTime>,
}

/// Valuation Precision Remainder: and the approved valuation table note.
#[model(
    name = "stock.valuation.remainder",
    table = "stk_value_remainder",
    description = "Valuation Precision Remainder",
    rec_name = "journal_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    log_access = false,
    constraints = [
        ("_valid_operand_value", r#"CHECK (CASE WHEN "operand_value" IS NULL THEN TRUE WHEN "operand_value" IS NOT NULL AND "operand_value" NOT IN ('NaN'::numeric, 'Infinity'::numeric, '-Infinity'::numeric) AND min_scale("operand_value") <= 18 AND abs("operand_value") < power(2::numeric, 256) THEN abs("operand_value" * power(10::numeric, 18)) < power(2::numeric, 256) ELSE FALSE END)"#, "operand_value must fit the 18-place valuation grid."),
        ("_valid_fraction", r#"CHECK (CASE WHEN "remainder_num" IS NOT NULL AND "remainder_num" NOT IN ('NaN'::numeric, 'Infinity'::numeric, '-Infinity'::numeric) AND "remainder_den" IS NOT NULL AND "remainder_den" NOT IN ('NaN'::numeric, 'Infinity'::numeric, '-Infinity'::numeric) AND min_scale("remainder_num") = 0 AND min_scale("remainder_den") = 0 AND "remainder_den" > 0 AND abs("remainder_num") < power(2::numeric, 4096) AND "remainder_den" < power(2::numeric, 4096) THEN gcd("remainder_num", "remainder_den") = 1 AND ("remainder_num" <> 0 OR "remainder_den" = 1) ELSE FALSE END)"#, "The valuation fraction must be reduced, within its bit limit and have a positive denominator."),
        ("_journal_company", "FOREIGN KEY (journal_id, company_id) REFERENCES stk_value_journal (id, company_id) ON DELETE RESTRICT", "A precision remainder must have its journal company."),
        ("_journal", "UNIQUE (journal_id)", "A journal contribution has one precision remainder."),
        ("_finite_operand_value", "CHECK (operand_value <> 'NaN' AND operand_value <> 'Infinity' AND operand_value <> '-Infinity')", "operand_value must be a finite number."),
        ("_finite_operand_quantity", "CHECK (operand_quantity <> 'NaN' AND operand_quantity <> 'Infinity' AND operand_quantity <> '-Infinity')", "operand_quantity must be a finite number."),
        ("_finite_operand_base_quantity", "CHECK (operand_base_quantity <> 'NaN' AND operand_base_quantity <> 'Infinity' AND operand_base_quantity <> '-Infinity')", "operand_base_quantity must be a finite number."),
        ("_finite_remainder_num", "CHECK (remainder_num <> 'NaN' AND remainder_num <> 'Infinity' AND remainder_num <> '-Infinity')", "remainder_num must be a finite number."),
        ("_finite_remainder_den", "CHECK (remainder_den <> 'NaN' AND remainder_den <> 'Infinity' AND remainder_den <> '-Infinity')", "remainder_den must be a finite number."),
    ],
)]
#[non_exhaustive]
pub struct ValuationRemainder {
    /// Journal id.
    #[field(
        many2one,
        comodel = "stock.valuation.journal",
        ondelete = "restrict",
        required = true
    )]
    pub journal_id: Option<RecId>,
    /// Company id.
    #[field(
        many2one,
        comodel = "res.company",
        ondelete = "restrict",
        required = true
    )]
    pub company_id: Option<RecId>,
    /// Policy.
    #[field(selection, selection = [("cost_scale_v1", "Cost Scale v1")], required = true)]
    pub policy: Option<Arc<str>>,
    /// Boundary.
    #[field(selection, selection = [("source_value", "Source Value"), ("outgoing_value", "Outgoing Value"), ("manual_revision", "Manual Revision")], required = true)]
    pub boundary: Option<Arc<str>>,
    /// Exact operand value.
    #[field(float, digits = 0)]
    pub operand_value: Decimal,
    /// Exact operand quantity.
    #[field(float, digits = 0)]
    pub operand_quantity: Decimal,
    /// Exact operand base quantity.
    #[field(float, digits = 0)]
    pub operand_base_quantity: Decimal,
    /// Exact remainder num.
    #[field(float, digits = 0, required = true)]
    pub remainder_num: Decimal,
    /// Exact remainder den.
    #[field(float, digits = 0, required = true)]
    pub remainder_den: Decimal,
    /// Recorded at (naive UTC).
    #[field(datetime, required = true, default_fn = recorded_now)]
    pub recorded_at: Option<DateTime>,
}
