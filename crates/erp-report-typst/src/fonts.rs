// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_report::{LimitKind, Limits, ReportError};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};
use typst::{foundations::Bytes, text::Font};

/// A font package that is not part of a release's addon files and is installed on a node separately.
///
/// Its manifest (families, file names, SHA-256) is compiled into the renderer like the default one;
/// the node supplies only the directory that holds the files
/// ([`TypstEngine::with_optional_fonts`](crate::TypstEngine::with_optional_fonts)).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptionalFonts {
    /// Noto Sans SC, TC, JP and KR (`optional-assets/report_fonts_cjk`).
    Cjk,
}
impl OptionalFonts {
    /// Every optional package this renderer knows.
    pub const ALL: [Self; 1] = [Self::Cjk];
    /// Directory name of the package, as used in messages and documentation.
    #[must_use]
    pub const fn package(self) -> &'static str {
        match self {
            Self::Cjk => "report_fonts_cjk",
        }
    }
    const fn manifest(self) -> &'static str {
        match self {
            Self::Cjk => include_str!("../../../optional-assets/report_fonts_cjk/fonts.json"),
        }
    }
    /// Families of the package, in manifest order.
    ///
    /// # Errors
    /// `Font` when the compiled manifest of the package cannot be read.
    pub fn families(self) -> Result<Vec<String>, ReportError> {
        let mut families: Vec<String> = Vec::new();
        for entry in parse(self.manifest())? {
            if !families.contains(&entry.family) {
                families.push(entry.family);
            }
        }
        Ok(families)
    }
}

#[derive(Deserialize)]
struct Entry {
    family: String,
    file: String,
    sha256: String,
}
fn parse(manifest: &str) -> Result<Vec<Entry>, ReportError> {
    serde_json::from_str(manifest).map_err(|_| ReportError::Font {
        source: None,
        reason: "invalid shipped font manifest".into(),
    })
}
/// One compiled manifest and the directory its files are read from.
struct FontSource {
    /// Directory name of the package, for messages.
    package: &'static str,
    /// `None` for the default package of a release.
    optional: Option<OptionalFonts>,
    /// Canonical directory; `None` while an optional package has not been given one.
    root: Option<PathBuf>,
    entries: Vec<Entry>,
}
/// The compiled manifests are the trust anchor, never a mutable manifest supplied by a document or
/// found in a font directory.
pub(crate) struct FontStore {
    limits: Limits,
    sources: Vec<FontSource>,
    loaded: Mutex<BTreeMap<String, Vec<Font>>>,
}
impl FontStore {
    pub fn new(root: &Path, limits: Limits) -> Result<Self, ReportError> {
        let mut sources = vec![FontSource {
            package: "report_fonts",
            optional: None,
            root: Some(root.canonicalize().map_err(|e| ReportError::Font {
                reason: "cannot open shipped font store".into(),
                source: Some(e),
            })?),
            entries: parse(include_str!("../../../addons/report_fonts/fonts.json"))?,
        }];
        for optional in OptionalFonts::ALL {
            sources.push(FontSource {
                package: optional.package(),
                optional: Some(optional),
                root: None,
                entries: parse(optional.manifest())?,
            });
        }
        // A family belongs to one package and a file name is listed once: a family is looked up
        // without naming its package, and the cache is keyed by file name.
        let (mut families, mut files) = (BTreeMap::new(), BTreeSet::new());
        for (index, source) in sources.iter().enumerate() {
            for entry in &source.entries {
                let owner = *families.entry(entry.family.as_str()).or_insert(index);
                if owner != index || !files.insert(entry.file.as_str()) {
                    return Err(ReportError::Font {
                        source: None,
                        reason: format!(
                            "invalid shipped font manifest: {:?} ({}) is listed twice",
                            entry.family, entry.file
                        ),
                    });
                }
            }
        }
        Ok(Self {
            limits,
            sources,
            loaded: Mutex::new(BTreeMap::new()),
        })
    }
    /// Gives an optional package its directory. The directory must exist; its files are read and
    /// checked on first use, like those of the default package.
    pub fn add_optional(
        &mut self,
        optional: OptionalFonts,
        root: &Path,
    ) -> Result<(), ReportError> {
        let package = optional.package();
        let source = self
            .sources
            .iter_mut()
            .find(|source| source.optional == Some(optional))
            .ok_or_else(|| ReportError::Font {
                source: None,
                reason: format!("optional font package {package} is not known to this renderer"),
            })?;
        if source.root.is_some() {
            return Err(ReportError::Font {
                source: None,
                reason: format!("optional font package {package} already has a directory"),
            });
        }
        let root = root.canonicalize().map_err(|e| ReportError::Font {
            reason: format!("cannot open optional font package {package}"),
            source: Some(e),
        })?;
        if !root.is_dir() {
            return Err(ReportError::Font {
                source: None,
                reason: format!("optional font package {package} is not a directory"),
            });
        }
        source.root = Some(root);
        Ok(())
    }
    /// File names parsed so far; a family that no document selected is absent.
    #[cfg(test)]
    pub fn loaded_files(&self) -> Vec<String> {
        let cache = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        cache.keys().cloned().collect()
    }
    pub fn select(&self, families: &[String]) -> Result<Vec<Font>, ReportError> {
        // An entry is written only after its file passed the size, digest and parse checks, so the map
        // is whole after a panic anywhere below: a poisoned lock is taken over.
        let mut cache = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        let mut selected = Vec::new();
        for family in families {
            let Some(source) = self
                .sources
                .iter()
                .find(|source| source.entries.iter().any(|e| e.family == *family))
            else {
                return Err(ReportError::Font {
                    source: None,
                    reason: format!(
                        "family {family:?} is not in the shipped font manifest; uploaded fonts require WorkerPool (a later release)"
                    ),
                });
            };
            let Some(root) = source.root.as_deref() else {
                // Only an optional package can be without a directory.
                let package = source.package;
                return Err(ReportError::Font {
                    source: None,
                    reason: format!(
                        "family {family:?} is in the optional font package {package}, which is not installed on this node"
                    ),
                });
            };
            for entry in source.entries.iter().filter(|e| e.family == *family) {
                if !cache.contains_key(&entry.file) {
                    cache.insert(entry.file.clone(), self.admit(source.package, root, entry)?);
                }
                let fonts = cache.get(&entry.file).ok_or_else(|| ReportError::Font {
                    source: None,
                    reason: "admitted font disappeared from cache".into(),
                })?;
                selected.extend(fonts.iter().cloned());
            }
        }
        Ok(selected)
    }
    /// Reads one manifest file from its package directory: inside the directory, within the size
    /// limit, with the compiled digest, and readable as a font.
    fn admit(&self, package: &str, root: &Path, entry: &Entry) -> Result<Vec<Font>, ReportError> {
        let font_io = |e| ReportError::Font {
            reason: format!("cannot read {} of font package {package}", entry.file),
            source: Some(e),
        };
        let path = root.join(&entry.file).canonicalize().map_err(font_io)?;
        if path.parent() != Some(root) {
            return Err(ReportError::Font {
                source: None,
                reason: "font asset escapes the configured store".into(),
            });
        }
        // Shipped, digest-admitted fonts only; this is the accepted 20 MiB asset ceiling.
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(font_io)?
            .take((self.limits.get(LimitKind::FontBytes) + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(font_io)?;
        if bytes.len() > self.limits.get(LimitKind::FontBytes) {
            return Err(ReportError::Font {
                source: None,
                reason: format!(
                    "shipped asset {} exceeds font_bytes {}",
                    entry.file,
                    self.limits.get(LimitKind::FontBytes)
                ),
            });
        }
        if Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            != entry.sha256
        {
            return Err(ReportError::Font {
                source: None,
                reason: format!(
                    "{} has a digest mismatch (font package {package})",
                    entry.file
                ),
            });
        }
        let fonts: Vec<_> = Font::iter(Bytes::new(bytes)).collect();
        if fonts.is_empty() {
            return Err(ReportError::Font {
                source: None,
                reason: format!("shipped asset {} has no readable font face", entry.file),
            });
        }
        Ok(fonts)
    }
}
