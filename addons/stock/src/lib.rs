//! Addon `stock` for ERPForAll.
//!
//! Following boundary §4.1 and Decision N8, `stock` manages warehouse topology,
//! quants, moves, pickings, and locations, but carries no valuation tables or fields.
//! Stock valuation belongs to the `stock_account` bridge addon.
//!
//! Installed so far: the topology tables (locations with their policy rows,
//! warehouses, operation types, routes, rules, storage categories, package types, removal
//! strategies and the topology version row), with immutable warehouse/operation-type company
//! ownership, stored location complete names and nearest-warehouse ownership, and a policy row created with each location; the tracking tables of
//! (lots, lot-name scope rows, packages, quants, serial positions), schema only;
//! the operations tables (references, transfers with their move counter
//! vector, moves with their product-unit conversion inputs), schema only; the move line tables of
//! (move lines, serial rows and correction rows on the lines' shared id
//! sequence), schema only;
//! and the replenishment configuration tables (orderpoints, putaway rules), schema only.
//!
//! Behaviour so far: the quant engine ([`quant_engine`]: signed balance changes in
//! key order, availability reads) with the quant model and location quant guards (`quant_guards`).
//! Company stock lookups and company-dependent contact/user preferences are declared by stock.
//! Provisioning remains a separate delivery; reading an empty company does not create topology.
//! Operation types resolve location defaults, configure standard sequences, maintain per-user favorites,
//! reschedule eligible move dates on method changes, and expose authorized basic transfer counts
//! and the overview card built from them.
//! Each transfer's move counter vector and state follow its moves in the statement that changes
//! them ([`move_counters`]). Open move lines take their move's defaults, hold their
//! reservation on their quant key and keep their move's quantity and state (`move_lines`).
//! Transfers check availability and unreserve (`reservation`, probe-then-lock), and
//! are validated in one balance change per call, after which their work is fixed (`done`).

erp_sdk::addon!(
    name = "stock",
    exact_only = true,
    depends = ["product", "barcodes_gs1_nomenclature"]
);

mod barcode;
pub mod error;
mod link_defaults;
pub mod location;
pub mod lot;
mod migrations;
mod operation_type;
mod operation_type_counters;
mod operation_type_dashboard;
mod operation_type_reservation;
pub mod orderpoint;
pub mod package;
pub mod picking;
mod picking_detailed_operations;
pub mod putaway;
pub mod quant;
pub mod reference;
mod res_company;
mod res_groups;
mod res_partner;
mod res_users;
pub mod route;
mod route_behavior;
mod route_onchange;
mod settings;
mod settings_panel;
pub mod stock_move;
pub mod stock_move_line;
pub mod storage;
pub mod tracking;
pub mod warehouse;
mod warehouse_setup;

pub use error::StockError;
pub use location::{ProductRemoval, StockLocation, StockLocationPolicy};
pub use lot::{StockLot, StockLotNameScope};
pub use orderpoint::StockWarehouseOrderpoint;
pub use package::StockPackage;
pub use picking::StockPicking;
pub use putaway::StockPutawayRule;
pub use quant::{StockQuant, StockSerialPosition};
pub use reference::StockReference;
pub use route::{StockRoute, StockRule, StockTopologyVersion};
pub use route_behavior::set_generated_identity;
pub use stock_move::StockMove;
pub use stock_move_line::{
    LOGICAL_ID_SEQUENCE, StockMoveLine, StockMoveLineCorrection, StockMoveLineSerial,
};
pub use storage::{StockPackageType, StockStorageCategory, StockStorageCategoryCapacity};
pub use warehouse::{StockPickingType, StockWarehouse};

// The quant engine and its guards: the one write path of `stock.quant` balances,
// the availability reads, and the location usage and archive guards over stock.
pub mod quant_engine;
mod quant_unit;
pub use quant_unit::{QuantRetryExhausted, QuantUnit, run_quant_unit};
mod quant_guards;
// Engine-owned move and line columns (done time, membership, correction counter), lead ruling L6'.
mod move_guards;
// The transfer's move counter vector and state: statement triggers, root locks and the
// cache notification after a move mutation.
mod move_counter_sql;
pub mod move_counters;
// Open move lines: defaults from the move, reservation per quant key, move quantity and state.
mod move_lines;
pub mod move_relations;
pub mod move_units;

// Reservation of moves from free stock and unreservation (check availability, unreserve).
mod reservation;

// Make-to-stock confirmation of moves and transfers, and the defaults a move takes from its transfer.
mod confirm;

// Validation of transfers: the done transition of moves and lines, and done work kept fixed.
mod done;

// The transfer's own rules: defaults and reference from the operation type, cancel, delete.
mod picking_flow;

pub use quant_engine::{
    QuantBalance, QuantChange, QuantError, QuantFilter, QuantKey, apply_quant_changes,
    apply_quant_changes_in_chunks, available_quantity, gather_quants,
};

/// Required SQL-object expectations for native registry/module composition.
pub mod schema_guards;
