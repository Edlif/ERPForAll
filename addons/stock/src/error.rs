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
    /// The negative-stock switch cannot be turned on while the location holds negative quants
    /// (1); correct them first.
    ForbidNegativeBlocked {
        /// The location whose policy row was written.
        location_id: i64,
        /// Quants of that exact location with a negative on-hand.
        negative: i64,
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
            } => write!(
                f,
                "Negative stock cannot be forbidden at location {location_id}: {negative} stock \
                 balances there are below zero. Correct them first."
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
            | StockError::GeneratedIdentity { .. } => Self::user(error.to_string()),
            StockError::DerivedFromMoves { .. }
            | StockError::ReferenceProjection { .. }
            | StockError::UnitSnapshot { .. } => Self::programming(error.to_string()),
            StockError::ForbidNegativeBlocked { .. }
            | StockError::TrackingChangeBlocked { .. }
            | StockError::ChainLimit { .. } => Self::validation(error.to_string()),
        };
        result.with_source(error)
    }
}
