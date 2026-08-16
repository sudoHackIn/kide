//! Language-neutral text documents and lexical matches for workspace search.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ByteRange, Fingerprint, TextPosition, WorkspacePath};

pub const MAX_TEXT_DOCUMENT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDocument {
    pub path: WorkspacePath,
    pub content: String,
    pub fingerprint: Fingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LexicalMatch {
    pub path: WorkspacePath,
    pub range: ByteRange,
    pub position: TextPosition,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextDocumentSkip {
    Binary,
    Oversized { bytes: usize },
    InvalidUtf8,
}

#[derive(Debug)]
pub struct WorkspaceTextInventory {
    pub documents: Vec<TextDocument>,
    pub skipped: Vec<(WorkspacePath, TextDocumentSkip)>,
}

/// Collects workspace files deterministically. Build output and VCS metadata
/// are excluded rather than silently treated as searchable source text.
pub fn collect_workspace_text(root: &Path) -> io::Result<WorkspaceTextInventory> {
    let root = fs::canonicalize(root)?;
    let mut paths = Vec::new();
    collect_files(&root, &root, &mut paths)?;
    paths.sort();
    let mut inventory = WorkspaceTextInventory {
        documents: Vec::new(),
        skipped: Vec::new(),
    };
    for path in paths {
        let relative = path
            .strip_prefix(&root)
            .expect("walker remains under root")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let workspace_path = WorkspacePath::new(relative);
        match fs::read(&path).map(|bytes| document_from_bytes(workspace_path.clone(), bytes))? {
            Ok(document) => inventory.documents.push(document),
            Err(skip) => inventory.skipped.push((workspace_path, skip)),
        }
    }
    Ok(inventory)
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_file() {
            output.push(path);
            continue;
        }
        if kind.is_dir()
            && !matches!(
                entry.file_name().to_str(),
                Some(
                    ".git"
                        | ".gradle"
                        | ".idea"
                        | ".kide"
                        | "build"
                        | "node_modules"
                        | "out"
                        | "target"
                )
            )
        {
            collect_files(root, &path, output)?;
        }
    }
    let _ = root;
    Ok(())
}

pub fn document_from_bytes(
    path: WorkspacePath,
    bytes: Vec<u8>,
) -> Result<TextDocument, TextDocumentSkip> {
    if bytes.len() > MAX_TEXT_DOCUMENT_BYTES {
        return Err(TextDocumentSkip::Oversized { bytes: bytes.len() });
    }
    if bytes.contains(&0) {
        return Err(TextDocumentSkip::Binary);
    }
    let content = String::from_utf8(bytes).map_err(|_| TextDocumentSkip::InvalidUtf8)?;
    let fingerprint = Fingerprint::new(format!("sha256:{:x}", Sha256::digest(content.as_bytes())));
    Ok(TextDocument {
        path,
        content,
        fingerprint,
    })
}

pub fn position_at(text: &str, offset: usize) -> Option<TextPosition> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Some(TextPosition {
        line,
        column: text[line_start..offset].chars().count() as u32 + 1,
    })
}

pub fn snippet_at(text: &str, start: usize, end: usize) -> String {
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = text[end..]
        .find('\n')
        .map_or(text.len(), |index| end + index);
    text[line_start..line_end].trim().to_owned()
}

pub fn lexical_terms(text: &str) -> Vec<(String, usize, usize)> {
    let mut terms = Vec::new();
    let mut start = None;
    for (offset, character) in text.char_indices() {
        if character.is_alphanumeric() || character == '_' {
            start.get_or_insert(offset);
        } else if let Some(start) = start.take() {
            terms.push((text[start..offset].to_owned(), start, offset));
        }
    }
    if let Some(start) = start {
        terms.push((text[start..].to_owned(), start, text.len()));
    }
    terms
}
