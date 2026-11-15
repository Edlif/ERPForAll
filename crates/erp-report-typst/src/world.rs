// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_report::ReportError;
use typst::{
    Library, LibraryExt, World,
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime, Dict, Duration},
    syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot},
    text::{Font, FontBook},
    utils::LazyHash,
};

pub(crate) struct ReportWorld {
    library: LazyHash<Library>,
    source: Source,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    date: Datetime,
}
impl ReportWorld {
    pub fn source(text: &str) -> Result<Source, ReportError> {
        let path = VirtualPath::new("/erp-print-v1.typ").map_err(|_| ReportError::Engine {
            reason: "the fixed renderer path is not a virtual path",
        })?;
        Ok(Source::new(
            FileId::new(RootedPath::new(VirtualRoot::Project, path)),
            text.into(),
        ))
    }
    pub fn new(source: Source, inputs: Dict, fonts: Vec<Font>, date: Datetime) -> Self {
        Self {
            library: LazyHash::new(Library::builder().with_inputs(inputs).build()),
            source,
            book: LazyHash::new(FontBook::from_fonts(&fonts)),
            fonts,
            date,
        }
    }
}
impl World for ReportWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }
    fn main(&self) -> FileId {
        self.source.id()
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::AccessDenied)
        }
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.source.id() {
            Ok(Bytes::from_string(self.source.text().to_owned()))
        } else {
            Err(FileError::AccessDenied)
        }
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }
    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        Some(self.date)
    }
}
