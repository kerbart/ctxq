//! rangex mechanics in the binary: `ingest` (inventory + conversion + repo
//! pointers, secrets quarantined, idempotent) and `export-map` (INDEX /
//! AGENTS / .gitignore from the front matter, refused while staging is not
//! empty).

use std::io::Write;
use std::path::Path;

use documentum::core::ingest::{export_map, ingest, IngestOptions, MapOptions};

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

fn write(p: &Path, body: impl AsRef<[u8]>) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn md_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".md"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn ingest_then_export_map_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let intake = dir.path().join("_vrac");
    let context = dir.path().join("context");

    write(
        &intake.join("CR réunion.md"),
        "# CR\n\nDécidé : pgvector.\n",
    );
    write(&intake.join("sub/notes.txt"), "Notes Atlas.\n");
    write(&intake.join("sub/notes (copie).txt"), "Notes Atlas.\n"); // duplicate by hash
    let doc = r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Contrat</w:t></w:r></w:p><w:p><w:r><w:t>Pénalité 2 %.</w:t></w:r></w:p></w:body></w:document>"#;
    write(
        &intake.join("Contrat Atlas.docx"),
        ooxml(&[("word/document.xml", doc)]),
    );
    // A key hidden inside an office document is only visible once converted.
    let leaky = r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>AKIAABCDEFGHIJKLMNOP</w:t></w:r></w:p></w:body></w:document>"#;
    write(
        &intake.join("Accès.docx"),
        ooxml(&[("word/document.xml", leaky)]),
    );
    write(
        &intake.join("service-account.json"),
        r#"{"type":"service_account","private_key":"x"}"#,
    );
    write(
        &intake.join("page.html"),
        "<h1>Page</h1><p>RGPD &amp; UE</p>",
    );
    write(
        &intake.join("mail.eml"),
        "From: a@x\nSubject: Prix\n\nTrois paliers.\n",
    );
    write(&intake.join("ancien.rtf"), "{\\rtf1 x}");
    write(&intake.join("logo.png"), b"\x89PNG\r\n\x1a\n");
    write(&intake.join("mon-repo/Cargo.toml"), "[package]\n");
    write(&intake.join("mon-repo/README.md"), "# mon-repo\n");
    write(&intake.join("mon-repo/src/main.rs"), "fn main() {}\n");

    let opts = IngestOptions {
        intake: intake.clone(),
        context: context.clone(),
        force: false,
    };
    let r = ingest(&opts).unwrap();
    assert_eq!(
        (r.kept, r.discarded_duplicates, r.repos, r.assets, r.secrets),
        (9, 1, 1, 1, 1),
        "{r:?}"
    );
    assert_eq!((r.converted, r.copied, r.failed), (5, 1, 1), "{r:?}");
    // One quarantined at inventory (name + content), one after conversion.
    assert_eq!(r.quarantined, 2, "{r:?}");
    assert!(
        r.quarantine
            .iter()
            .any(|(p, k)| p.ends_with("Accès.docx") && k == "aws-access-key"),
        "{:?}",
        r.quarantine
    );
    assert!(
        r.failures
            .iter()
            .any(|(p, why)| p.ends_with("ancien.rtf") && why.contains("MarkItDown")),
        "{:?}",
        r.failures
    );

    let staging = context.join(".organizer/staging");
    let names = md_names(&staging);
    assert_eq!(names.len(), 5, "{names:?}");
    assert!(
        names.iter().all(|n| n.ends_with("_cr-r-union.md")
            || n.contains("_notes")
            || n.ends_with("_contrat-atlas.md")
            || n.ends_with("_page.md")
            || n.ends_with("_mail.md")),
        "{names:?}"
    );
    let contrat = std::fs::read_to_string(
        staging.join(
            names
                .iter()
                .find(|n| n.ends_with("_contrat-atlas.md"))
                .unwrap(),
        ),
    )
    .unwrap();
    assert!(
        contrat.starts_with("---\ntitle: \"Contrat Atlas.docx\"\nsource_path: "),
        "{contrat}"
    );
    assert!(
        contrat.contains("source_type: \"docx\"")
            && contrat.contains("category: \"divers\"")
            && contrat.contains("Pénalité 2 %."),
        "{contrat}"
    );
    for leaked in ["AKIA", "private_key"] {
        for n in &names {
            assert!(
                !std::fs::read_to_string(staging.join(n))
                    .unwrap()
                    .contains(leaked),
                "{leaked} leaked into {n}"
            );
        }
    }
    assert!(context.join("assets/logo.png").exists());
    let pointer = md_names(&context.join("sources/divers"));
    assert_eq!(pointer.len(), 1);
    let pointer_md =
        std::fs::read_to_string(context.join("sources/divers").join(&pointer[0])).unwrap();
    assert!(
        pointer_md.contains("source_type: repo")
            && pointer_md.contains("Rust (1)")
            && pointer_md.contains("main.rs"),
        "{pointer_md}"
    );

    // Inventory JSON keeps rangex's shape.
    let inv: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(context.join(".organizer/inventory.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(inv["counts"]["kept"], 9);
    assert_eq!(inv["discarded"][0]["reason"], "duplicate-hash");
    assert!(inv["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["secret"] == true && f["secret_reason"] == "gcp-service-account"));

    // Idempotent: nothing re-converted, nothing duplicated.
    let again = ingest(&opts).unwrap();
    assert_eq!(
        (again.converted, again.copied, again.skipped),
        (0, 0, 7),
        "{again:?}"
    );
    assert_eq!(md_names(&staging).len(), 5);
    assert_eq!(md_names(&context.join("sources/divers")).len(), 1);
    // --force re-converts into the same files.
    let forced = ingest(&IngestOptions {
        force: true,
        ..IngestOptions {
            intake: intake.clone(),
            context: context.clone(),
            force: true,
        }
    })
    .unwrap();
    assert_eq!(forced.converted, 5);
    assert_eq!(md_names(&staging).len(), 5);

    // The map is refused while documents wait in staging.
    let map_opts = MapOptions {
        context: context.clone(),
        profile: "shaping".into(),
        intake: "_vrac".into(),
        force: false,
        agents_template: None,
    };
    let refused = export_map(&map_opts).unwrap();
    assert!(
        !refused.written && refused.leftover_staging.len() == 5,
        "{refused:?}"
    );
    assert!(!context.join("INDEX.md").exists());

    // The agent classifies (moves + enriches), then the map builds.
    for n in md_names(&staging) {
        let body = std::fs::read_to_string(staging.join(&n)).unwrap();
        let (cat, summary) = if n.ends_with("_cr-r-union.md") {
            ("reunions", "CR : pgvector retenu.")
        } else {
            ("specs", "Spec.")
        };
        let body = body
            .replace("category: \"divers\"", &format!("category: \"{cat}\""))
            .replace("summary: \"\"", &format!("summary: \"{summary}\""))
            .replace("tags: []", "tags: [atlas]");
        write(&context.join("sources").join(cat).join(&n), body);
        std::fs::remove_file(staging.join(&n)).unwrap();
    }
    let built = export_map(&map_opts).unwrap();
    assert!(
        built.written && built.docs == 6 && built.categories == 3,
        "{built:?}"
    );
    let index = std::fs::read_to_string(context.join("INDEX.md")).unwrap();
    assert!(
        index.starts_with(
            "# INDEX — project context map\n\n> **6 document(s)** · profile **`shaping`**"
        ),
        "{index}"
    );
    assert!(
        index.contains("## Specs & scoping (4)")
            && index.contains("## Meetings / minutes (1)")
            && index.contains("## Misc (1)"),
        "{index}"
    );
    assert!(
        index.contains("— CR : pgvector retenu. `#atlas`"),
        "{index}"
    );
    let index_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(context.join("INDEX.json")).unwrap())
            .unwrap();
    assert_eq!(index_json["count"], 6);
    assert_eq!(index_json["categories"][0]["key"], "specs");
    let agents = std::fs::read_to_string(context.join("AGENTS.md")).unwrap();
    assert!(
        agents.contains("profile **`shaping`** · 6 document(s)"),
        "{agents}"
    );
    assert_eq!(std::fs::read_to_string(context.join(".gitignore")).unwrap(), "# rangex — never push potentially confidential content\n/_vrac/\nsources/\nassets/\n.organizer/\n");
}
