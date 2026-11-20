// SPDX-License-Identifier: MIT OR Apache-2.0
//! AI provider configuration, explicit company routing and offline request encoding.
erp_sdk::addon!(name = "ai_base", depends = ["base"], exact_only = true);

pub mod config;
pub mod encoding;
pub mod models;
pub mod routing;
mod settings;

#[erp_sdk::__private::linkme::distributed_slice(erp_sdk::views::FIELD_WIDGETS)]
#[linkme(crate = erp_sdk::__private::linkme)]
static MODEL_LIST_WIDGET: erp_sdk::views::WidgetDecl =
    erp_sdk::views::WidgetDecl::new("ai_base.model_list", &["text"]);
