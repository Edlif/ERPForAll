// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::error::{Result, invalid};
use crate::{Limit, PdfLimits};
/// Non-empty, ordered, distinct one-based page numbers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PageSelection(pub(crate) Vec<u32>);
impl PageSelection {
    /// Validate page numbers; bounds against the actual document are checked by each operation.
    pub fn new(pages: Vec<u32>, limits: &PdfLimits) -> Result<Self> {
        limits.check(Limit::Pages, pages.len())?;
        if pages.is_empty() {
            return Err(invalid("PDF page selection is empty"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &pages {
            if *p == 0 {
                return Err(invalid("PDF page numbers start at 1"));
            }
            if !seen.insert(*p) {
                return Err(invalid(format!("PDF page selection repeats page {p}")));
            }
        }
        Ok(Self(pages))
    }
    /// Selected page numbers in output order.
    pub fn pages(&self) -> &[u32] {
        &self.0
    }
}
/// Normalized clockwise quarter turns.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Rotation(pub(crate) i32);
impl Rotation {
    /// Accept multiples of 90, including negative rotations.
    pub fn new(degrees: i32) -> Result<Self> {
        if degrees % 90 != 0 {
            return Err(invalid(format!(
                "PDF rotation must be a multiple of 90 degrees; got {degrees}"
            )));
        }
        Ok(Self(degrees.rem_euclid(360)))
    }
}
/// Affine placement in target default page coordinates (before page rotation).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StampPlacement(pub(crate) [f32; 6]);
impl StampPlacement {
    /// Matrix `[a,b,c,d,e,f]`; values must be finite and non-singular.
    pub fn new(matrix: [f32; 6]) -> Result<Self> {
        let determinant = f64::from(matrix[0]) * f64::from(matrix[3])
            - f64::from(matrix[1]) * f64::from(matrix[2]);
        if matrix.iter().any(|x| !x.is_finite()) || determinant == 0.0 {
            return Err(invalid("PDF stamp matrix must be finite and non-singular"));
        }
        Ok(Self(matrix))
    }
}
/// What a reading copy (merge, extract, split) does with page annotations.
///
/// A reading copy has a fresh catalog: no form and no name trees. An annotation is
/// *self-contained* when it needs none of them: a link to a URI or to a page of the same copy,
/// and the markup types (notes, highlights, shapes, ink, stamps, free text with its own
/// appearance) with their popups and replies. An annotation that belongs to a layer is carried
/// with its group, which the copy lists in its rebuilt optional-content configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum AnnotationPolicy {
    /// Refuse a document that has any page annotation. The default: nothing is lost unseen.
    #[default]
    Refuse,
    /// Carry self-contained annotations; refuse the document when it has any other.
    KeepSelfContained,
    /// Carry self-contained annotations and leave the others out; the summary counts both.
    KeepOrDrop,
}
