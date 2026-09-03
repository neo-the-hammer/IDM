//! File categories.
//!
//! IDM sorts finished downloads into Documents, Music, Video, Programs and
//! Compressed folders by file type, and users rely on it heavily enough that
//! arriving without it would feel like a missing feature rather than a
//! simplification. Categories are user-editable: the defaults below are only a
//! starting point.

use hdm_json::{json, Json};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Category {
    /// Stable identifier used by the API and the saved download list.
    pub id: String,
    /// Display name; the UI localizes the built-in ones by `id`.
    pub name: String,
    /// Lowercase extensions, without a leading dot.
    pub extensions: Vec<String>,
    /// Where files of this category are saved. Relative paths are resolved
    /// against the default download directory.
    pub directory: Option<PathBuf>,
    /// Built-in categories cannot be deleted, only edited.
    pub builtin: bool,
}

impl Category {
    pub fn matches(&self, filename: &str) -> bool {
        let Some(extension) = extension_of(filename) else {
            return false;
        };
        self.extensions.contains(&extension)
    }

    pub fn to_json(&self) -> Json {
        json!({
            "id": (self.id.as_str()),
            "name": (self.name.as_str()),
            "extensions": (Json::Arr(
                self.extensions.iter().map(|e| Json::Str(e.clone())).collect()
            )),
            "directory": (self.directory.as_ref().map(|d| d.to_string_lossy().into_owned())),
            "builtin": (self.builtin)
        })
    }

    pub fn from_json(value: &Json) -> Option<Category> {
        Some(Category {
            id: value.get("id")?.as_str()?.to_string(),
            name: value.get("name")?.as_str()?.to_string(),
            extensions: value
                .get("extensions")?
                .as_arr()?
                .iter()
                .filter_map(|e| e.as_str().map(|s| s.to_ascii_lowercase()))
                .collect(),
            directory: value
                .get("directory")
                .and_then(Json::as_str)
                .map(PathBuf::from),
            builtin: value
                .get("builtin")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        })
    }
}

/// The lowercase extension of a filename, without the dot.
pub fn extension_of(filename: &str) -> Option<String> {
    let name = filename.rsplit(['/', '\\']).next()?;
    let (_, extension) = name.rsplit_once('.')?;
    if extension.is_empty() || extension.len() > 16 {
        return None;
    }
    Some(extension.to_ascii_lowercase())
}

/// The default category set, matching what users expect from IDM plus the
/// formats that have become common since.
pub fn defaults() -> Vec<Category> {
    let make = |id: &str, name: &str, folder: &str, extensions: &[&str]| Category {
        id: id.to_string(),
        name: name.to_string(),
        extensions: extensions.iter().map(|e| e.to_string()).collect(),
        directory: Some(PathBuf::from(folder)),
        builtin: true,
    };
    vec![
        make(
            "compressed",
            "Compressed",
            "Compressed",
            &[
                "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "tgz", "txz", "cab", "arj",
                "lzh", "ace", "iso", "img", "dmg",
            ],
        ),
        make(
            "documents",
            "Documents",
            "Documents",
            &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf",
                "txt", "md", "epub", "mobi", "azw3", "djvu", "csv",
            ],
        ),
        make(
            "music",
            "Music",
            "Music",
            &[
                "mp3", "flac", "wav", "aac", "ogg", "oga", "opus", "m4a", "wma", "aiff", "ape",
                "mid", "midi",
            ],
        ),
        make(
            "video",
            "Video",
            "Video",
            &[
                "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "3gp",
                "ts", "m2ts", "ogv", "vob",
            ],
        ),
        make(
            "programs",
            "Programs",
            "Programs",
            &[
                "exe", "msi", "msix", "appx", "deb", "rpm", "pkg", "apk", "appimage", "bat", "sh",
                "run",
            ],
        ),
        make(
            "images",
            "Images",
            "Images",
            &[
                "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "tif", "tiff", "heic", "avif",
                "ico", "psd", "raw",
            ],
        ),
    ]
}

/// The category set, with lookup.
#[derive(Debug, Clone)]
pub struct Categories {
    categories: Vec<Category>,
    /// Where relative category directories are rooted, and where uncategorized
    /// files go.
    pub root: PathBuf,
    /// When false, everything lands in `root` regardless of type.
    pub enabled: bool,
}

impl Categories {
    pub fn new(root: PathBuf) -> Categories {
        Categories {
            categories: defaults(),
            root,
            enabled: true,
        }
    }

    pub fn all(&self) -> &[Category] {
        &self.categories
    }

    pub fn set_all(&mut self, categories: Vec<Category>) {
        self.categories = categories;
    }

    pub fn get(&self, id: &str) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// The category a filename belongs to, if any.
    pub fn classify(&self, filename: &str) -> Option<&Category> {
        if !self.enabled {
            return None;
        }
        self.categories.iter().find(|c| c.matches(filename))
    }

    /// The directory a file should be saved in.
    ///
    /// An explicitly chosen category wins over the extension, because a user
    /// who picked one meant it.
    pub fn directory_for(&self, filename: &str, forced: Option<&str>) -> PathBuf {
        let category = match forced {
            Some(id) => self.get(id),
            None => self.classify(filename),
        };
        match category.and_then(|c| c.directory.as_ref()) {
            Some(dir) if dir.is_absolute() => dir.clone(),
            Some(dir) => self.root.join(dir),
            None => self.root.clone(),
        }
    }

    pub fn to_json(&self) -> Json {
        json!({
            "enabled": (self.enabled),
            "root": (self.root.to_string_lossy().into_owned()),
            "categories": (Json::Arr(self.categories.iter().map(Category::to_json).collect()))
        })
    }

    pub fn from_json(value: &Json, fallback_root: &Path) -> Categories {
        let categories = value
            .get("categories")
            .and_then(Json::as_arr)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Category::from_json)
                    .collect::<Vec<_>>()
            })
            .filter(|c: &Vec<Category>| !c.is_empty())
            .unwrap_or_else(defaults);
        Categories {
            categories,
            root: value
                .get("root")
                .and_then(Json::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| fallback_root.to_path_buf()),
            enabled: value.get("enabled").and_then(Json::as_bool).unwrap_or(true),
        }
    }
}
