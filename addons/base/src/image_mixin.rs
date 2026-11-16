// SPDX-License-Identifier: MIT OR Apache-2.0
//! `image.mixin`: the abstract mixin that gives a model one picture and four stored smaller sizes.
//!
//! Our own
//! implementation on the `image` field kind (column-backed mode). A model gets the five
//! fields by inheriting the mixin (`inherit = [ImageMixin]`): guide.
//!
//! Writing `image_1920` fills the four sizes; a size is never written on its own. A picture that
//! already fits a size is stored in that size as it came, never enlarged.

use erp_sdk::prelude::*;

/// The image mixin: `image_1920` and its stored sizes.
#[model(name = "image.mixin", description = "Image Mixin", kind = "abstract")]
pub struct ImageMixin {
    /// The picture, at most 1920 x 1920 pixels.
    #[field(image, max_width = 1920, max_height = 1920, string = "Image")]
    pub image_1920: Option<Blob>,
    /// The picture at most 1024 x 1024 pixels, stored.
    #[field(
        image,
        max_width = 1024,
        max_height = 1024,
        related = "image_1920",
        store = true,
        string = "Image 1024"
    )]
    pub image_1024: Option<Blob>,
    /// The picture at most 512 x 512 pixels, stored.
    #[field(
        image,
        max_width = 512,
        max_height = 512,
        related = "image_1920",
        store = true,
        string = "Image 512"
    )]
    pub image_512: Option<Blob>,
    /// The picture at most 256 x 256 pixels, stored.
    #[field(
        image,
        max_width = 256,
        max_height = 256,
        related = "image_1920",
        store = true,
        string = "Image 256"
    )]
    pub image_256: Option<Blob>,
    /// The picture at most 128 x 128 pixels, stored.
    #[field(
        image,
        max_width = 128,
        max_height = 128,
        related = "image_1920",
        store = true,
        string = "Image 128"
    )]
    pub image_128: Option<Blob>,
}
