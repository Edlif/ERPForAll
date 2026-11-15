// SPDX-License-Identifier: MIT OR Apache-2.0
// a constraint is sealed by default, so `wire` on it is a contradiction
struct Thing;

#[erp_macros::methods(Thing)]
impl Thing {
    #[method(wire)]
    #[constrains("qty")]
    async fn _check_qty(this: &u8, tx: &mut u8) {}
}

fn main() {}
