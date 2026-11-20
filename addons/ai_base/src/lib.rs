// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit AI provider and company routing configuration. No provider transport is implemented.
erp_sdk::addon!(name = "ai_base", depends = ["base"], exact_only = true);

pub mod config;
pub mod models;
pub mod routing;
