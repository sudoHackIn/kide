use kide_core::{document_from_bytes, position_at, IndexStore, TextDocumentSkip, WorkspacePath};
use tempfile::tempdir;

#[test]
fn syncs_changed_and_deleted_documents_with_unicode_locations() {
    let directory = tempdir().expect("temporary index directory");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens store");
    let alpha = document_from_bytes(
        WorkspacePath::new("alpha.txt"),
        "one 😀 needle\ntwo".as_bytes().to_vec(),
    )
    .expect("text document");
    let beta = document_from_bytes(WorkspacePath::new("beta.txt"), b"needle".to_vec())
        .expect("text document");
    store
        .sync_text_documents(&[alpha, beta])
        .expect("syncs inventory");
    std::fs::write(directory.path().join("alpha.txt"), "one 😀 needle\ntwo").expect("writes alpha");
    std::fs::write(directory.path().join("beta.txt"), "needle").expect("writes beta");
    let matches = store
        .lexical_matches(directory.path(), "needle")
        .expect("searches inventory");
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].path.as_str(), "alpha.txt");
    assert_eq!(matches[0].position.line, 1);
    assert_eq!(matches[0].position.column, 7);
    assert_eq!(matches[1].path.as_str(), "beta.txt");

    let changed = document_from_bytes(WorkspacePath::new("alpha.txt"), b"changed".to_vec())
        .expect("text document");
    store
        .sync_text_documents(&[changed])
        .expect("replaces complete inventory");
    assert!(store
        .lexical_matches(directory.path(), "needle")
        .expect("searches updated inventory")
        .is_empty());
}

#[test]
fn rejects_binary_and_oversized_documents_explicitly() {
    assert_eq!(
        document_from_bytes(WorkspacePath::new("binary"), vec![0, 1]),
        Err(TextDocumentSkip::Binary)
    );
    assert!(matches!(
        document_from_bytes(
            WorkspacePath::new("large"),
            vec![b'a'; kide_core::MAX_TEXT_DOCUMENT_BYTES + 1]
        ),
        Err(TextDocumentSkip::Oversized { .. })
    ));
    assert_eq!(position_at("😀x", 4).expect("unicode boundary").column, 2);
}
