// SPDX-License-Identifier: MIT OR Apache-2.0
// `wire` and `sealed` contradict each other
struct Thing;

#[erp_macros::methods(Thing)]
impl Thing {
    #[method(wire, sealed)]
    async fn lend(this: &u8, tx: &mut u8) {}
}

fn main() {}
