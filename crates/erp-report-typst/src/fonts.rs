// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_report::{LimitKind, Limits, ReportError};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};
use typst::{foundations::Bytes, text::Font};

#[derive(Deserialize)]
struct Entry {
    family: String,
    file: String,
    sha256: String,
}
/// The compiled manifest is the trust anchor, never a mutable manifest supplied by a document.
pub(crate) struct FontStore {
    root: PathBuf,
    limits: Limits,
    entries: Vec<Entry>,
    loaded: Mutex<BTreeMap<String, Vec<Font>>>,
}
impl FontStore {
    pub fn new(root: &Path, limits: Limits) -> Result<Self, ReportError> {
        let entries = serde_json::from_str(include_str!("../../../addons/report_fonts/fonts.json"))
            .map_err(|_| ReportError::Font {
                source: None,
                reason: "invalid shipped font manifest".into(),
            })?;
        Ok(Self {
            limits,
            root: root.canonicalize().map_err(|e| ReportError::Font {
                reason: "cannot open shipped font store".into(),
                source: Some(e),
            })?,
            entries,
            loaded: Mutex::new(BTreeMap::new()),
        })
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
            let entries: Vec<_> = self
                .entries
                .iter()
                .filter(|e| e.family == *family)
                .collect();
            if entries.is_empty() {
                return Err(ReportError::Font {
                    source: None,
                    reason: format!(
                        "family {family:?} is not in the shipped font manifest; uploaded fonts require WorkerPool (a later release)"
                    ),
                });
            }
            for entry in entries {
                if !cache.contains_key(&entry.file) {
                    let font_io = |e| ReportError::Font {
                        reason: format!("cannot read shipped asset {}", entry.file),
                        source: Some(e),
                    };
                    let path = self
                        .root
                        .join(&entry.file)
                        .canonicalize()
                        .map_err(font_io)?;
                    if path.parent() != Some(self.root.as_path()) {
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
                            reason: format!("shipped asset {} has a digest mismatch", entry.file),
                        });
                    }
                    let fonts: Vec<_> = Font::iter(Bytes::new(bytes)).collect();
                    if fonts.is_empty() {
                        return Err(ReportError::Font {
                            source: None,
                            reason: format!(
                                "shipped asset {} has no readable font face",
                                entry.file
                            ),
                        });
                    }
                    cache.insert(entry.file.clone(), fonts);
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
}
