// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Detailed Operations page of a transfer: its move lines (the other side of
//! `stock.move.line.picking_id`). No column; line writes keep the move line model's own checks.
use crate::picking::StockPicking;
use erp_sdk::prelude::*;

#[extend(StockPicking)]
pub(crate) struct StockPickingMoveLines {
    /// Move lines of the transfer.
    #[field(
        one2many,
        comodel = "stock.move.line",
        inverse_name = "picking_id",
        readonly = true,
        copy = false,
        string = "Operations"
    )]
    pub move_line_ids: Vec<RecId>,
}
