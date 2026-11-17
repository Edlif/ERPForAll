// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed failures of the stock addon.

use std::error::Error as StdError;
use std::fmt;

/// A stock request the addon refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StockError {
    /// Company scrap/default stock links are computed outputs.
    CompanyLocationsDerived,
    /// Existing warehouses and operation types keep their company, even while archived.
    /// Archive the record and create a replacement in the desired company instead.
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::CompanyChangeForbidden.into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::UserError);
    /// ```
    CompanyChangeForbidden,
    /// Location warehouse membership is maintained from the tree and warehouse roots.
    LocationOwnershipDerived,
    /// The negative-stock switch cannot be turned on while the location holds negative quants or
    /// keys with more reserved than on hand (1); correct them first.
    ForbidNegativeBlocked {
        /// The location whose policy row was written.
        location_id: i64,
        /// Quants of that exact location with a negative on-hand.
        negative: i64,
        /// Other quants of that location with more reserved than on hand.
        over: i64,
    },
    /// A tracking change would leave lot or serial rows the new mode cannot hold (6).
    TrackingChangeBlocked {
        /// Display name of the product.
        product: String,
        /// Lots, serial numbers or open serial rows found.
        count: i64,
    },
    /// An ORM or RPC create or write named a column only the stock engine writes (done time,
    /// membership, correction counter; lead ruling L6').
    EngineOnlyField {
        /// `stock move` or `stock move line`.
        what: &'static str,
        /// The label of the field written.
        field: String,
    },
    /// A create carried a `default_<field>` context key for one of those columns.
    EngineOnlyDefault {
        /// `stock move` or `stock move line`.
        what: &'static str,
        /// The label of the field the key names.
        field: String,
    },
    /// A create or write named a column of a transfer that its moves decide (the counter vector
    /// and the state,.3), or a create carried a `default_<field>` context
    /// key for one. Refused for every caller, superuser included.
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::DerivedFromMoves {
    ///     field: "Status".into(),
    ///     default: false,
    /// }
    /// .into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::Programming);
    /// ```
    DerivedFromMoves {
        /// The label of the field named.
        field: String,
        /// Named by a context default rather than in the values.
        default: bool,
    },
    /// An ORM upsert of transfers by their declared key. The generic upsert runs no `create` or
    /// `write` override, so it would skip every transfer guard; refused for every caller.
    TransferUpsert,
    /// A create or write named a column of a stock move that its operation lines, confirmation and
    /// reservation decide (status, quantity), or a create carried a `default_<field>` context key
    /// for one. Refused for every caller, superuser included.
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::DerivedFromLines {
    ///     field: "Status".into(),
    ///     default: false,
    /// }
    /// .into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::Programming);
    /// ```
    DerivedFromLines {
        /// The label of the field named.
        field: String,
        /// Named by a context default rather than in the values.
        default: bool,
    },
    /// A create or write named the reference set of a transfer or the transfers of a reference:
    /// both follow from the references of the moves (topology ADR D3), or a create carried a
    /// `default_<field>` context key for one. Refused for every caller, superuser included.
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::ReferenceProjection {
    ///     field: "References".into(),
    ///     default: false,
    /// }
    /// .into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::Programming);
    /// ```
    ReferenceProjection {
        /// The label of the field named.
        field: String,
        /// Named by a context default rather than in the values.
        default: bool,
    },
    /// A deadline change would reach more linked open moves than the system parameter
    /// `stock.move_chain_limit` admits. Nothing is changed.
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::ChainLimit { limit: 3 }.into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::Validation);
    /// ```
    ChainLimit {
        /// The limit in force.
        limit: i64,
    },
    /// A create or write named a unit snapshot column of a stock move (factors, digits, product-unit
    /// quantities) with a value other than the unit service's conversion (6).
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::UnitSnapshot {
    ///     field: "Unit Factor".into(),
    /// }
    /// .into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::Programming);
    /// ```
    UnitSnapshot {
        /// The label of the field named.
        field: String,
    },
    /// A move line named a lot, a lot name or serial rows: lots on operation lines come with.
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::LotsNotAvailable.into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::UserError);
    /// ```
    LotsNotAvailable,
    /// A move line was created without a move; a line that creates its move comes with.
    LineNeedsMove,
    /// A move line was added to a done or cancelled move.
    LineMoveClosed,
    /// A done move line was written or deleted; corrections of done work come with a later slice.
    DoneLineChange,
    /// A create or write of a done move or a done transfer named a field outside the allow-list
    /// of the stock `done` module (quantities, product, locations, state, links), or a move was added to
    /// a done transfer.
    ///
    /// ```
    /// let error: erp_sdk::Error = erp_addon_stock::StockError::DoneRecordFixed {
    ///     what: "stock move",
    ///     field: "Demand".into(),
    /// }
    /// .into();
    /// assert_eq!(error.kind(), erp_sdk::ErrorKind::UserError);
    /// ```
    DoneRecordFixed {
        /// `stock move` or `transfer`.
        what: &'static str,
        /// The label of the field named.
        field: String,
    },
    /// A done move or a done transfer was deleted.
    DoneDelete {
        /// `stock move` or `transfer`.
        what: &'static str,
    },
    /// Validate named only transfers that are done already.
    AlreadyDone,
    /// Validate named a cancelled transfer.
    ValidateCancelled,
    /// Cancel named a done transfer.
    CancelDone,
    /// Cancel named a transfer without moves: its state follows from its moves, so an
    /// empty transfer stays a draft; it is deleted instead.
    CancelEmpty,
    /// Validate named a transfer without products (E-empty).
    ValidateEmpty,
    /// Validate named a transfer whose moves have no quantity (
    /// E-zero).
    ValidateNoQuantity,
    /// Validate would leave part of a demand open, which needs a backorder (a later slice).
    BackorderNotAvailable {
        /// Display name of the first product short of its demand.
        product: String,
    },
    /// Cancelling a move whose destination moves are still open needs cancel propagation along
    /// the chain (a later slice).
    CancelChained,
    /// A move line write named a column that follows the line's move (move, transfer, company,
    /// product).
    LineRelink {
        /// The label of the field named.
        field: String,
    },
    /// Check availability on transfers that have no move to reserve.
    NothingToAssign,
    /// Confirm on transfers that have no draft move (the reference does nothing; refused so the
    /// person sees why nothing happened).
    NothingToConfirm,
    /// Confirming a make-to-order move, or one whose rule falls back to an order, needs the
    /// procurement engine.
    NeedsProcurement,
    /// Unreserve named a done move.
    UnreserveDone,
    /// A create or write named the generated identity of a route or rule (warehouse, addon, role),
    /// or a create carried a `default_<field>` context key for it. Only the warehouse setup that
    /// generates the record sets it, through [`crate::set_generated_identity`].
    GeneratedIdentity {
        /// `route` or `rule`.
        what: &'static str,
        /// The label of the field named.
        field: String,
    },
}

impl fmt::Display for StockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CompanyLocationsDerived => f.write_str("Company scrap and default stock locations are derived and cannot be assigned."),
            Self::CompanyChangeForbidden => f.write_str(
                "The company of this record cannot be changed once it exists. Archive it and create a new record for the other company."
            ),
            Self::LocationOwnershipDerived => f.write_str(
                "Location warehouse ownership is derived from its hierarchy and cannot be assigned."
            ),
            Self::ForbidNegativeBlocked {
                location_id,
                negative,
                over,
            } => write!(
                f,
                "Negative stock cannot be forbidden at location {location_id}: {negative} stock \
                 balances there are below zero and {over} other balances have more reserved than \
                 on hand. Correct them first."
            ),
            Self::TrackingChangeBlocked { product, count } => write!(
                f,
                "Tracking of {product} cannot be changed while it has stock or open operations with \
                 lots or serial numbers ({count} found). Converting existing stock is not available \
                 yet (a later release)."
            ),
            Self::EngineOnlyField { what, field } => write!(
                f,
                "{field} of a {what} is recorded by the stock engine when the operation is \
                 completed or corrected; it cannot be set directly."
            ),
            Self::EngineOnlyDefault { what, field } => write!(
                f,
                "{field} of a {what} is recorded by the stock engine; a new {what} cannot take \
                 it from a default in the context."
            ),
            Self::DerivedFromMoves {
                field,
                default: false,
            } => write!(
                f,
                "{field} of a transfer follows from its moves; it cannot be set directly."
            ),
            Self::DerivedFromMoves {
                field,
                default: true,
            } => write!(
                f,
                "{field} of a transfer follows from its moves; a new transfer cannot take it \
                 from a default in the context."
            ),
            Self::TransferUpsert => write!(
                f,
                "Transfers cannot be updated by key: the checks of a transfer run on create and \
                 write. Use create or write."
            ),
            Self::DerivedFromLines {
                field,
                default: false,
            } => write!(
                f,
                "{field} of a stock move follows from its detailed operations, its confirmation \
                 and its reservation; it cannot be set directly."
            ),
            Self::DerivedFromLines {
                field,
                default: true,
            } => write!(
                f,
                "{field} of a stock move follows from its detailed operations, its confirmation \
                 and its reservation; a new stock move cannot take it from a default in the \
                 context."
            ),
            Self::ReferenceProjection {
                field,
                default: false,
            } => write!(
                f,
                "{field} follows from the references of the moves; set the references on the moves \
                 instead."
            ),
            Self::ReferenceProjection {
                field,
                default: true,
            } => write!(
                f,
                "{field} follows from the references of the moves; a new record cannot take it from \
                 a default in the context."
            ),
            Self::ChainLimit { limit } => write!(
                f,
                "This deadline change would reach more than {limit} linked moves (system parameter \
                 stock.move_chain_limit); nothing was changed."
            ),
            Self::UnitSnapshot { field } => write!(
                f,
                "{field} of a stock move is converted from its unit and product by the unit \
                 service; it cannot be set to another value."
            ),
            Self::LotsNotAvailable => f.write_str(
                "Lots and serial numbers on operation lines are not available yet (a later release); leave them empty."
            ),
            Self::LineNeedsMove => f.write_str(
                "An operation line belongs to a stock move; add the product to the transfer's \
                 operations first. Lines that create their own move are not available yet."
            ),
            Self::LineMoveClosed => {
                f.write_str("Operation lines cannot be added to a done or cancelled move.")
            }
            Self::DoneLineChange => f.write_str(
                "A done operation line cannot be changed or deleted. Corrections of done work \
                 are not available yet."
            ),
            Self::DoneRecordFixed { what, field } => write!(
                f,
                "{field} of a done {what} cannot be changed: its stock has already moved."
            ),
            Self::DoneDelete { what } => write!(
                f,
                "A done {what} cannot be deleted: its stock has already moved."
            ),
            Self::AlreadyDone => f.write_str("Nothing to validate: the transfer was validated earlier."),
            Self::ValidateCancelled => f.write_str("A cancelled transfer cannot be validated."),
            Self::CancelDone => f.write_str("A done transfer cannot be cancelled."),
            Self::CancelEmpty => f.write_str(
                "A transfer without products cannot be cancelled; delete it instead."
            ),
            Self::ValidateEmpty => f.write_str(
                "A transfer without products has nothing to validate; add the products to move."
            ),
            Self::ValidateNoQuantity => f.write_str(
                "There is no quantity to validate: check availability or set the quantities of \
                 the operation lines first."
            ),
            Self::BackorderNotAvailable { product } => write!(
                f,
                "{product} is short of its demand. Validating part of a transfer creates a \
                 backorder for the rest, and backorders are not available yet: set the \
                 quantities to the demand, lower the demand, or set the operation type to never \
                 create backorders."
            ),
            Self::CancelChained => f.write_str(
                "This move feeds another open operation; cancelling along such a chain is not \
                 available yet."
            ),
            Self::LineRelink { field } => write!(
                f,
                "{field} of an operation line follows its stock move and cannot be changed; \
                 delete the line and add it on the other move."
            ),
            Self::NothingToAssign => f.write_str(
                "There is nothing to check the availability for: confirm the transfer first, or \
                 add products to it."
            ),
            Self::NothingToConfirm => f.write_str(
                "There is nothing to confirm: add products to the transfer first, or it is \
                 already confirmed."
            ),
            Self::NeedsProcurement => f.write_str(
                "A move that is supplied by order cannot be confirmed yet: procurement comes with \
                 task a later release. Set its supply method to take from stock."
            ),
            Self::UnreserveDone => {
                f.write_str("A done stock move cannot be unreserved; its stock has already moved.")
            }
            Self::GeneratedIdentity { what, field } => write!(
                f,
                "{field} of a stock {what} is filled in only when a warehouse generates the \
                 {what}; it cannot be set directly or from a default."
            ),
        }
    }
}

impl StdError for StockError {}

impl From<StockError> for erp_sdk::Error {
    fn from(error: StockError) -> Self {
        let result = match &error {
            StockError::CompanyLocationsDerived
            | StockError::CompanyChangeForbidden
            | StockError::LocationOwnershipDerived
            | StockError::EngineOnlyField { .. }
            | StockError::EngineOnlyDefault { .. }
            | StockError::GeneratedIdentity { .. }
            | StockError::LotsNotAvailable
            | StockError::LineNeedsMove
            | StockError::LineMoveClosed
            | StockError::DoneLineChange
            | StockError::DoneRecordFixed { .. }
            | StockError::DoneDelete { .. }
            | StockError::AlreadyDone
            | StockError::ValidateCancelled
            | StockError::CancelDone
            | StockError::CancelEmpty
            | StockError::ValidateEmpty
            | StockError::ValidateNoQuantity
            | StockError::BackorderNotAvailable { .. }
            | StockError::CancelChained
            | StockError::LineRelink { .. }
            | StockError::NothingToAssign
            | StockError::UnreserveDone
            | StockError::NothingToConfirm
            | StockError::NeedsProcurement => Self::user(error.to_string()),
            StockError::DerivedFromMoves { .. }
            | StockError::TransferUpsert
            | StockError::DerivedFromLines { .. }
            | StockError::ReferenceProjection { .. }
            | StockError::UnitSnapshot { .. } => Self::programming(error.to_string()),
            StockError::ForbidNegativeBlocked { .. }
            | StockError::TrackingChangeBlocked { .. }
            | StockError::ChainLimit { .. } => Self::validation(error.to_string()),
        };
        result.with_source(error)
    }
}
