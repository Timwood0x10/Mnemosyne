//! Pride and Prejudice full-corpus English frontend regression.
//!
//! Source: Project Gutenberg eBook #1342 by Jane Austen. The corpus file keeps
//! the complete Gutenberg header and license notice for provenance.

use mnemosyne::compiler::CompileContext;
use mnemosyne::compiler::document::Document;
use mnemosyne::compiler::entity::EntityDictionary;
use mnemosyne::compiler::{chunk, profile, sentence};
use mnemosyne::language::EnglishLanguageProvider;

/// Objective: Verify the English frontend compiles a second independent full novel.
/// Invariants: The complete corpus is substantial and title discovery finds named people.
#[test]
fn pride_prejudice_full_english_frontend_regression() {
    let document = Document::from_file("corpus/PrideAndPrejudice.txt")
        .expect("Pride and Prejudice corpus must be readable");
    assert!(
        document.text.len() > 700_000,
        "The regression must use the complete public-domain novel, got {} bytes",
        document.text.len()
    );
    assert!(
        document
            .text
            .contains("Project Gutenberg eBook of Pride and Prejudice"),
        "The corpus must retain its Project Gutenberg provenance header"
    );

    let chunks = chunk::plan(&document.text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    assert!(
        sentences.len() > 5_000,
        "The full English corpus should produce more than 5,000 sentences, got {}",
        sentences.len()
    );

    let mut context = CompileContext {
        document_title: "Pride and Prejudice".to_string(),
        ..Default::default()
    };
    profile::extract_profiles(
        &document.text,
        &mut context,
        Some(&EntityDictionary::default()),
        &[],
        &EnglishLanguageProvider::new(),
    );

    assert!(
        context.entities.len() >= 10,
        "English title discovery should find at least ten people in Pride and Prejudice, got {}: {:?}",
        context.entities.len(),
        context
            .entities
            .iter()
            .take(20)
            .map(|entity| entity.name.as_str())
            .collect::<Vec<_>>()
    );
    for expected in ["Mr. Bennet", "Mr. Bingley", "Mr. Darcy"] {
        assert!(
            context
                .entities
                .iter()
                .any(|entity| entity.name == expected),
            "English title discovery must find canonical entity `{expected}`; found {:?}",
            context
                .entities
                .iter()
                .take(30)
                .map(|entity| entity.name.as_str())
                .collect::<Vec<_>>()
        );
    }
    assert!(
        context
            .profiles
            .iter()
            .any(|profile| profile.key == "title"),
        "English entities should retain evidence-backed title profiles"
    );
}
