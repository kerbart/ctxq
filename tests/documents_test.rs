//! P2b gate: office documents are searchable, front matter drives filters,
//! and every recorded file is accounted for.

use std::io::Write;

use ctxq::core::docmeta::SearchFilters;
use ctxq::core::hybrid::DocEngine;

fn ooxml(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in parts {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

fn sync(engine: &mut DocEngine, root: &std::path::Path) {
    engine.sync_directory_incremental(root, None).unwrap();
}

#[test]
fn documents_front_matter_filters_and_completeness() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("kickoff.md"),
        "---\ntitle: Kickoff\nsummary: Lancement du projet Atlas.\ntags: [atlas, planning]\ndate: 2026-06-20\n---\n# Kickoff\n\n## Jalons\n\nLivraison du lot Atlas en septembre.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("other.md"),
        "# Notes\n\n## Atlas\n\nLe projet Atlas utilise PostgreSQL.\n",
    )
    .unwrap();
    std::fs::write(root.join("empty.md"), "   \n").unwrap();
    let doc = r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Contrat Atlas</w:t></w:r></w:p><w:p><w:r><w:t>Pénalité de retard : 2 % par semaine.</w:t></w:r></w:p></w:body></w:document>"#;
    std::fs::write(
        root.join("contrat.docx"),
        ooxml(&[("word/document.xml", doc)]),
    )
    .unwrap();
    std::fs::write(root.join("broken.pptx"), b"not a zip").unwrap();

    let mut engine = DocEngine::in_memory().unwrap();
    sync(&mut engine, root);

    // The docx is indexed through its markdown conversion.
    let hits = engine.search("pénalité de retard du contrat", 3).unwrap();
    assert!(hits.iter().any(|h| h.source == "contrat.docx"), "{hits:?}");
    let docx = hits.iter().find(|h| h.source == "contrat.docx").unwrap();
    assert_eq!(docx.meta.as_ref().unwrap().source_type, "docx");

    // Front matter: returned with the hit, and usable as a facet.
    let tagged = engine
        .search_filtered(
            "projet Atlas",
            5,
            &SearchFilters {
                tags: vec!["planning".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !tagged.is_empty() && tagged.iter().all(|h| h.source == "kickoff.md"),
        "{tagged:?}"
    );
    assert_eq!(
        tagged[0].meta.as_ref().unwrap().summary.as_deref(),
        Some("Lancement du projet Atlas.")
    );
    let only_docx = engine
        .search_filtered(
            "Atlas",
            5,
            &SearchFilters {
                source_types: vec!["docx".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!only_docx.is_empty() && only_docx.iter().all(|h| h.source == "contrat.docx"));
    let recent = engine
        .search_filtered(
            "Atlas",
            5,
            &SearchFilters {
                since: Some("2026-07-01".into()),
                tags: vec!["atlas".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(recent.is_empty(), "kickoff is dated 2026-06-20: {recent:?}");

    // Every recorded file is accounted for.
    let c = engine.completeness(root).unwrap();
    assert_eq!(c.indexed, 3, "{c:?}");
    assert_eq!(c.empty, 1, "{c:?}");
    assert!(
        c.unusable
            .iter()
            .any(|(p, why)| p == "broken.pptx" && why.contains("conversion")),
        "{c:?}"
    );
    assert!(c.unaccounted.is_empty(), "{c:?}");
}
