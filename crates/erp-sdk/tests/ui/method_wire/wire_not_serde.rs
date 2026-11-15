// SPDX-License-Identifier: MIT OR Apache-2.0
// `wire` asserts a wire form: an argument without serde fails the build naming the method.
use erp_sdk::prelude::*;

#[model(name = "test.wire.bad", addon = "test_wire")]
pub struct Bad {
    #[field(char)]
    pub name: Option<Arc<str>>,
}

/// No serde implementation.
pub struct Opaque(pub u8);

#[methods(Bad, addon = "test_wire")]
impl Bad {
    #[method(wire)]
    async fn lend(_this: &RecordSet<Bad>, _tx: &mut Transaction, value: Opaque) -> Result<i64> {
        Ok(i64::from(value.0))
    }
}

fn main() {}
