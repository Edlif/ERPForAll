// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stock balances (`stock.quant`) and serial positions (`stock.serial.position`): tables.
//! Both are projections of the stock ledger (2). Quants are written
//! only by [`crate::quant_engine`]; the ORM refuses generic writes of their balance
//! columns (`crate::quant_guards`). Serial positions get their writer
//! in a later slice, after.

use erp_sdk::DefaultContext;
use erp_sdk::prelude::*;

fn now(_ctx: &DefaultContext) -> Value {
    Value::DateTime(DateTime::now())
}

/// `stock.quant`: the balance of one key `(company, product, location, lot, package, owner)`.
///
/// One row per key: the key is `UNIQUE NULLS NOT DISTINCT` (2,.1). The
/// reserved counter is never negative and never clamped (1). The company is the company
/// of the ledger line that created the key, stored and required, never derived from the location
/// (B6). Quantities are exact `numeric` in the product unit. The product
/// declares its company check (its company comes through the template delegation); lot, location,
/// owner and assigned user are checked by the framework default.
///
/// The lot belongs to the quant's product: the composite foreign key `(lot_id, product_id)` →
/// `stock_lot (id, product_id)` (`stock_quant_lot_product`) refuses another product's lot for every
/// writer, ORM or SQL; an untracked quant (no lot) is not checked (S1).
#[model(
    name = "stock.quant",
    description = "Quants",
    rec_name = "product_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    keys = [key(
        name = "_key",
        fields = ["company_id", "product_id", "location_id", "lot_id", "package_id", "owner_id"],
        message = "A stock balance already exists for this company, product, location, lot, package and owner."
    )],
    constraints = [
        (
            "_reserved_nonneg",
            "CHECK (reserved_quantity >= 0)",
            "The reserved quantity of a stock balance cannot be negative."
        ),
        // named `finite_*` so PostgreSQL, which tests CHECKs in name order, reports a NaN here first
        (
            "_finite_quantity",
            "CHECK (quantity <> 'NaN' AND quantity <> 'Infinity' AND quantity <> '-Infinity')",
            "The quantity of a stock balance must be a finite number."
        ),
        (
            "_finite_reserved_quantity",
            "CHECK (reserved_quantity <> 'NaN' AND reserved_quantity <> 'Infinity' AND reserved_quantity <> '-Infinity')",
            "The reserved quantity of a stock balance must be a finite number."
        ),
        (
            "_finite_inventory_quantity",
            "CHECK (inventory_quantity <> 'NaN' AND inventory_quantity <> 'Infinity' AND inventory_quantity <> '-Infinity')",
            "The counted quantity of a stock balance must be a finite number."
        ),
        // the lot belongs to the row's product (raw until slice B6 declares composite keys);
        // needs `stock_lot_id_product`, created first because `lot.rs` sorts before this file
        (
            "_lot_product",
            "FOREIGN KEY (lot_id, product_id) REFERENCES stock_lot (id, product_id)",
            "The lot or serial number of a stock balance belongs to another product."
        ),
    ],
    // FIFO and LIFO removal order read without a sort or a heap visit (plan I3 W-I3.04); the
    // benefit is unmeasured (DEFERRED_PERFORMANCE)
    indexes = [index(
        name = "_removal_order",
        fields = ["company_id", "product_id", "location_id", "in_date", "id"],
        include = ["quantity", "reserved_quantity", "lot_id", "package_id", "owner_id"]
    )],
)]
pub struct StockQuant {
    /// Location and the identifying lot, package and owner of this balance.
    #[field(char, compute = "_compute_quant_name")]
    pub display_name: Option<Arc<str>>,

    /// Company of the ledger line that created the key (B6); leads the key, so it needs no
    /// index of its own.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict"
    )]
    pub company_id: Option<RecId>,
    /// Product.
    #[field(
        many2one,
        comodel = "product.product",
        required = true,
        ondelete = "restrict",
        index = true,
        check_company = true
    )]
    pub product_id: Option<RecId>,
    /// Exact location of the balance.
    #[field(
        many2one,
        comodel = "stock.location",
        required = true,
        ondelete = "restrict",
        index = true
    )]
    pub location_id: Option<RecId>,
    /// Lot or serial number; empty for untracked stock.
    #[field(
        many2one,
        comodel = "stock.lot",
        ondelete = "restrict",
        index = "btree_not_null",
        string = "Lot/Serial Number"
    )]
    pub lot_id: Option<RecId>,
    /// Package holding the stock; empty for loose stock.
    #[field(
        many2one,
        comodel = "stock.package",
        ondelete = "restrict",
        index = "btree_not_null",
        string = "Package"
    )]
    pub package_id: Option<RecId>,
    /// Owner of consigned stock.
    #[field(
        many2one,
        comodel = "res.partner",
        ondelete = "restrict",
        index = "btree_not_null",
        string = "Owner"
    )]
    pub owner_id: Option<RecId>,
    #[field(
        float,
        digits = "Product Unit",
        required = true,
        readonly = true,
        string = "Quantity"
    )]
    pub quantity: Decimal,
    /// Quantity reserved by open move lines on this key; never negative, never clamped.
    #[field(
        float,
        digits = "Product Unit",
        required = true,
        readonly = true,
        default = 0,
        string = "Reserved Quantity"
    )]
    pub reserved_quantity: Decimal,
    /// Signed free quantity, generated by PostgreSQL from current balances for individual and grouped reads.
    #[field(
        float,
        digits = "Product Unit",
        generated = "quantity - reserved_quantity",
        readonly = true,
        string = "Available Quantity"
    )]
    pub available_quantity: Decimal,
    /// Current quantity for the applied-count read path. Count adjustments use the inventory workflow.
    #[field(
        float,
        digits = "Product Unit",
        related = "quantity",
        readonly = true,
        string = "Applied Quantity"
    )]
    pub inventory_quantity_auto_apply: Decimal,
    /// Whether this balance belongs to the selected warehouse/location scope.
    #[field(
        boolean,
        compute = "_compute_on_hand",
        search_method = "_search_on_hand",
        readonly = true,
        string = "On Hand"
    )]
    pub on_hand: bool,
    /// Unit of the product, shown beside the quantities (a quant has no unit of its own; not stored).
    #[field(
        many2one,
        comodel = "uom.uom",
        related = "product_id.uom_id",
        string = "Unit"
    )]
    pub uom_id: Option<RecId>,
    /// Whether a printed Count Sheet shows the on-hand quantity: the system parameter
    /// `stock.show_expected_quantity_count` (off: a blind count). Not stored; the report template
    /// cannot read parameters, so the quant carries the switch.
    #[field(
        boolean,
        compute = "_compute_show_expected_count",
        string = "Show Expected Count"
    )]
    pub show_expected_count: bool,
    /// Incoming date, the FIFO order of the key.
    #[field(
        datetime,
        required = true,
        readonly = true,
        default_fn = now,
        string = "Incoming Date"
    )]
    pub in_date: Option<DateTime>,
    /// Counted quantity of an inventory count (row state, never written by transitions).
    #[field(float, digits = "Product Unit", string = "Counted")]
    pub inventory_quantity: Decimal,
    /// Whether `inventory_quantity` has been explicitly entered for the current count.
    #[field(boolean, default = false, string = "Counted Set")]
    pub inventory_quantity_set: bool,
    /// Signed difference between `inventory_quantity` and `quantity` while a count is set.
    #[field(
        float,
        digits = "Product Unit",
        compute = "_compute_inventory_diff_quantity",
        readonly = true,
        string = "Difference"
    )]
    pub inventory_diff_quantity: Decimal,
    /// Next scheduled physical count date (derived from location cycle frequency or annual count date).
    #[field(date, string = "Scheduled Date")]
    pub inventory_date: Option<Date>,
    /// Optional note explaining an inventory count adjustment.
    #[field(char, string = "Adjustment Note")]
    pub inventory_note: Option<Arc<str>>,
    /// User assigned to count this balance.
    #[field(many2one, comodel = "res.users", string = "Assigned To")]
    pub user_id: Option<RecId>,
}

/// `stock.serial.position`: the summed quantity of one serial at one exact location across packages
/// and owners (1). `quantity <= 1` is the serial rule, with no lower bound (D5.2).
///
/// The serial belongs to the position's product: the composite foreign key `(lot_id, product_id)` →
/// `stock_lot (id, product_id)` (`stock_serial_position_lot_product`), without which one serial could
/// hold a position under two products at one location and escape `_max_one` (S1).
#[model(
    name = "stock.serial.position",
    description = "Serial Number Position",
    log_access = false,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    keys = [key(
        name = "_key",
        fields = ["company_id", "product_id", "lot_id", "location_id"],
        message = "A serial number has one position per company and location."
    )],
    constraints = [
        (
            "_max_one",
            "CHECK (quantity <= 1)",
            "A serial number cannot be more than once at one location."
        ),
        (
            "_finite_quantity",
            "CHECK (quantity <> 'NaN' AND quantity <> 'Infinity' AND quantity <> '-Infinity')",
            "The quantity of a serial position must be a finite number."
        ),
        // the serial belongs to the row's product (raw until slice B6); needs
        // `stock_lot_id_product`, created first because `lot.rs` sorts before this file
        (
            "_lot_product",
            "FOREIGN KEY (lot_id, product_id) REFERENCES stock_lot (id, product_id)",
            "The serial number of a serial position belongs to another product."
        ),
    ],
)]
pub struct StockSerialPosition {
    /// Company of the quant keys the position sums.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict"
    )]
    pub company_id: Option<RecId>,
    /// Serial-tracked product.
    #[field(
        many2one,
        comodel = "product.product",
        required = true,
        ondelete = "restrict",
        check_company = true
    )]
    pub product_id: Option<RecId>,
    /// The serial number.
    #[field(
        many2one,
        comodel = "stock.lot",
        required = true,
        ondelete = "restrict",
        index = true
    )]
    pub lot_id: Option<RecId>,
    /// Exact location.
    #[field(
        many2one,
        comodel = "stock.location",
        required = true,
        ondelete = "restrict",
        index = true
    )]
    pub location_id: Option<RecId>,
    /// Summed quantity of the serial at the location.
    #[field(float, digits = "Product Unit", required = true)]
    pub quantity: Decimal,
}
