// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixtures of Binary and Image fields (column-backed mode): one model with a plain
//! binary field, an unbounded image, a bounded image, a stored size related to it and two editable
//! aliases of it (one an image, one a plain binary). Load with `base` (the image limits are its system parameters).

use erp_sdk::prelude::*;

erp_sdk::addon!(name = "test_image", depends = ["base"]);

/// A record with file content and pictures.
#[model(
    name = "test_image.picture",
    description = "Picture",
    addon = "test_image"
)]
pub struct Picture {
    #[field(char)]
    pub name: Option<Arc<str>>,
    /// Any file content.
    #[field(binary)]
    pub document: Option<Blob>,
    /// A picture stored as uploaded (type and pixel checks only).
    #[field(image)]
    pub original: Option<Blob>,
    /// A picture stored at most 1920 pixels on each edge.
    #[field(image, max_width = 1920, max_height = 1920)]
    pub image_1920: Option<Blob>,
    /// The stored small size of `image_1920`.
    #[field(
        image,
        max_width = 128,
        max_height = 128,
        related = "image_1920",
        store = true
    )]
    pub image_128: Option<Blob>,
    /// An editable alias of `image_1920` without bounds of its own: a write stores the picture
    /// under the bounds of `image_1920`.
    #[field(image, related = "image_1920", readonly = false)]
    pub photo: Option<Blob>,
    /// An editable alias of `image_1920` declared as a plain binary: no image bounds of its own,
    /// so only its target makes a write load the image limits.
    #[field(binary, related = "image_1920", readonly = false)]
    pub picture: Option<Blob>,
}
