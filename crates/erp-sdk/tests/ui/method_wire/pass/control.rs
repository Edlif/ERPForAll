// SPDX-License-Identifier: MIT OR Apache-2.0
// A method whose argument has no serde implementation compiles without `wire` (it stays
// native-only), `wire` on a serialisable method compiles, and `sealed` parses alone and with
// `override`.
use erp_sdk::prelude::*;

#[model(name = "test.wire.control", addon = "test_wire")]
pub struct Control {
    #[field(char)]
    pub name: Option<Arc<str>>,
}

/// No serde implementation.
pub struct Opaque(pub u8);

#[methods(Control, addon = "test_wire")]
impl Control {
    #[method]
    async fn native_only(
        _this: &RecordSet<Control>,
        _tx: &mut Transaction,
        value: Opaque,
    ) -> Result<Opaque> {
        Ok(value)
    }

    #[method(wire)]
    async fn plain(
        _this: &RecordSet<Control>,
        _tx: &mut Transaction,
        ids: Vec<i64>,
        label: Option<String>,
    ) -> Result<(i64, bool)> {
        Ok((ids.len() as i64, label.is_some()))
    }

    #[method(sealed)]
    async fn _guarded(_this: &RecordSet<Control>, _tx: &mut Transaction) -> Result<()> {
        Ok(())
    }

    #[method(override, sealed)]
    async fn unlink(this: &RecordSet<Control>, tx: &mut Transaction, sup: Super) -> Result<()> {
        sup.call(tx, this.as_records(), ()).await
    }
}

fn main() {}
