// SPDX-License-Identifier: MIT OR Apache-2.0
//! Image bounds on a computed or related image field are kept only when the field is stored: the
//! ORM scales a picture to its bounds when it stores it, so bounds on a field read unscaled are
//! refused by name rather than ignored.
#![allow(clippy::unwrap_used, reason = "tests")]

use erp_registry::{AddonDescriptor, FieldDecl, ModelDecl, RegistryBuilder};

static ADDON: AddonDescriptor = AddonDescriptor {
    name: "image_decl",
    depends: &[],
    exact_only: false,
};

fn build(field: FieldDecl) -> erp_fields::Result<erp_registry::Registry> {
    let fields =
        Box::leak(vec![FieldDecl::image("image_1920", 1920, 1920), field].into_boxed_slice());
    let model = Box::leak(Box::new(
        ModelDecl::new("image_decl", "image_decl.item").fields(fields),
    ));
    RegistryBuilder::new([&ADDON]).with_decls([&*model]).build()
}

#[test]
fn a_bounded_related_image_must_be_stored() {
    let error = build(FieldDecl::image("image_128", 128, 128).related("image_1920")).unwrap_err();
    assert_eq!(
        error.message(),
        "field 'image_decl.item.image_128': a computed or related image with max_width or \
         max_height must be stored (store = true); a non-stored one is read unscaled"
    );
    let registry = build(
        FieldDecl::image("image_128", 128, 128)
            .related("image_1920")
            .store(true),
    )
    .unwrap();
    assert!(
        registry
            .model("image_decl.item")
            .unwrap()
            .field("image_128")
            .is_some()
    );
    // an unbounded related image reads its source as it is, which is what it declares
    build(FieldDecl::image("photo", 0, 0).related("image_1920")).unwrap();
}
