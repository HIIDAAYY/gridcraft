//! Where a workbook is, in words the user knows.
//!
//! Android hands the app `content://` addresses, never paths, and GridCraft edits a workbook from
//! storage in a private working copy ([`crate::workcopies`]) whose path means nothing to the
//! user. This keeps, for each address a workbook was opened from or saved to, its name and folder
//! ("penjualan.xlsx" in "Download/GridCraft-tes"), so File › Info shows where the workbook is and
//! the Recent list can name the files it reopens. The addresses persist (`places.json` in the
//! profile) with the Recent list; working-copy paths are linked to their address for this run.
//!
//! Pure data handling, tested on the desktop.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Most addresses remembered (the Recent list keeps 20).
const MAX_PLACES: usize = 64;

/// The name and folder of each known address (and working copy).
#[derive(Default)]
pub struct Places {
    /// Address → (name, folder); persisted.
    known: BTreeMap<String, (String, String)>,
    /// Working-copy path → its address; this run only.
    copies: BTreeMap<String, String>,
    /// Where `known` is kept.
    file: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    places: BTreeMap<String, (String, String)>,
}

impl Places {
    /// The places kept in `file` (missing or unreadable: none).
    pub fn load(file: PathBuf) -> Self {
        let known = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Stored>(&b).ok()).map(|s| s.places).unwrap_or_default();
        Self { known, copies: BTreeMap::new(), file: Some(file) }
    }

    /// Remember that `uri` holds the workbook called `name`.
    pub fn note(&mut self, uri: &str, name: &str) {
        let folder = folder_of(uri);
        self.known.insert(uri.to_string(), (name.to_string(), folder));
        if self.known.len() > MAX_PLACES
            && let Some(first) = self.known.keys().next().cloned()
            && first != uri
        {
            self.known.remove(&first);
        }
        self.store();
    }

    /// The working copy at `path` is the workbook at `uri`.
    pub fn link(&mut self, path: &str, uri: &str) {
        self.copies.insert(path.to_string(), uri.to_string());
    }

    /// (name, folder) for an address or a working-copy path the app knows.
    pub fn get(&self, key: &str) -> Option<(String, String)> {
        let uri = self.copies.get(key).map_or(key, String::as_str);
        self.known.get(uri).cloned()
    }

    /// Forget the addresses that are no longer in `keep` (the Recent list) and not open.
    pub fn keep_only(&mut self, keep: &[String]) {
        let open: Vec<&String> = self.copies.values().collect();
        let before = self.known.len();
        self.known.retain(|uri, _| keep.contains(uri) || open.contains(&uri));
        if self.known.len() != before {
            self.store();
        }
    }

    fn store(&self) {
        let Some(file) = &self.file else { return };
        let Ok(json) = serde_json::to_vec(&Stored { places: self.known.clone() }) else { return };
        let tmp = file.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, file)) {
            log::warn!("places: cannot save {}: {e}", file.display());
        }
    }
}

/// The folder a `content://` address names, as the user sees it in the file picker:
/// `…/document/primary%3ADownload%2FReports%2Fq3.xlsx` → `Download/Reports`. Providers whose
/// addresses carry no path are named after the place they stand for, or nothing.
pub fn folder_of(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("content://") else { return String::new() };
    let authority = rest.split('/').next().unwrap_or("");
    match authority {
        "com.android.providers.downloads.documents" => return "Download".into(),
        "com.google.android.apps.docs.storage" => return "Google Drive".into(),
        _ => {}
    }
    if authority != "com.android.externalstorage.documents" {
        return String::new();
    }
    let Some(document) = rest.rsplit("/document/").next().filter(|d| !d.is_empty() && *d != rest) else { return String::new() };
    let decoded = percent_decode(document);
    // "primary:Download/x.xlsx" (the device), "1A2B-3C4D:Download/x.xlsx" (an SD card).
    let (volume, path) = decoded.split_once(':').unwrap_or(("", decoded.as_str()));
    let folder = path.rsplit_once('/').map_or("", |(f, _)| f);
    match volume {
        "primary" | "" => folder.to_string(),
        _ if folder.is_empty() => "SD card".into(),
        _ => format!("SD card/{folder}"),
    }
}

/// `%XX` escapes decoded (malformed ones kept as they are); not UTF-8: replaced characters.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) = (bytes.get(i + 1).and_then(|c| hex(*c)), bytes.get(i + 2).and_then(|c| hex(*c)))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_read_from_storage_addresses() {
        let device = "content://com.android.externalstorage.documents/document/primary%3ADownload%2FGridCraft-tes%2Fpenjualan.xlsx";
        assert_eq!(folder_of(device), "Download/GridCraft-tes");
        let tree = "content://com.android.externalstorage.documents/tree/primary%3ADownload/document/primary%3ADownload%2Fq3.xlsx";
        assert_eq!(folder_of(tree), "Download");
        assert_eq!(folder_of("content://com.android.externalstorage.documents/document/primary%3Aq3.xlsx"), "");
        assert_eq!(folder_of("content://com.android.externalstorage.documents/document/1A2B-3C4D%3AWork%2Fq3.xlsx"), "SD card/Work");
        assert_eq!(folder_of("content://com.android.providers.downloads.documents/document/msf%3A1000001234"), "Download");
        assert_eq!(folder_of("content://com.google.android.apps.docs.storage/document/acc%3D1%3Bdoc%3D9"), "Google Drive");
        assert_eq!(folder_of("content://media/external/file/12"), "");
        assert_eq!(folder_of("/data/user/0/x/files/docs/1/q3.xlsx"), "");
        // Hostile escapes never panic.
        assert_eq!(folder_of("content://com.android.externalstorage.documents/document/primary%3AA%2%ZZ%E2%82%2Fb.xlsx"), "A%2%ZZ\u{fffd}");
    }

    #[test]
    fn places_name_addresses_and_their_working_copies_and_persist() {
        let dir = std::env::temp_dir().join(format!("gridcraft-places-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("places.json");
        let uri = "content://com.android.externalstorage.documents/document/primary%3ADownload%2Fq3.xlsx";
        let mut places = Places::load(file.clone());
        places.note(uri, "q3.xlsx");
        places.link("/data/docs/7/q3.xlsx", uri);
        assert_eq!(places.get(uri), Some(("q3.xlsx".into(), "Download".into())));
        assert_eq!(places.get("/data/docs/7/q3.xlsx"), Some(("q3.xlsx".into(), "Download".into())));
        assert_eq!(places.get("/elsewhere.xlsx"), None);
        let again = Places::load(file.clone());
        assert_eq!(again.get(uri), Some(("q3.xlsx".into(), "Download".into())), "kept across a restart");
        assert_eq!(again.get("/data/docs/7/q3.xlsx"), None, "working copies are this run's");
        // Dropped from Recent and not open: forgotten.
        let mut again = again;
        again.keep_only(&[]);
        assert_eq!(Places::load(file).get(uri), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
