// SPDX-License-Identifier: MIT OR Apache-2.0
//! `crm.tag`: a label for sales documents.

use erp_sdk::prelude::*;

/// `crm.tag`: a named, coloured label; names are unique.
#[model(
    name = "crm.tag",
    description = "CRM Tag",
    order = "name, id",
    constraints = [("_name_uniq", "unique(name)", "Each tag needs its own name; choose a name no other tag uses.")]
)]
pub struct CrmTag {
    /// Tag name.
    #[field(char, required = true, translate = true, string = "Tag Name")]
    pub name: Option<Arc<str>>,
    /// Colour index of the badge (1 to 11), picked at random on create as the reference does.
    #[field(integer, default_fn = crate::team::random_color, string = "Color")]
    pub color: i32,
}
