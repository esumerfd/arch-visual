//! `Model::revision` identifies a model's content, so a renderer can cache
//! everything it derives from a model and rebuild only when the revision it
//! cached against no longer matches.

use seam_core::{apply_batch, from_json, GraphEvent};

const TWO_NODES: &str = r#"{
  "nodes": [
    {"id": "a", "label": "A", "community": 0, "source_file": "src/a.rs"},
    {"id": "b", "label": "B", "community": 1, "source_file": "src/b.rs"}
  ],
  "links": []
}"#;

fn add_node(id: &str, label: &str) -> GraphEvent {
    GraphEvent::AddNode {
        id: id.to_string(),
        label: label.to_string(),
        community: Some("0".to_string()),
        source_file: Some(format!("src/{id}.rs")),
    }
}

#[test]
fn two_separately_loaded_models_never_share_a_revision() {
    let first = from_json(TWO_NODES).unwrap().model;
    let second = from_json(TWO_NODES).unwrap().model;
    assert_ne!(
        first.revision, second.revision,
        "loading a new graph must never look like the one already cached"
    );
}

#[test]
fn a_clone_keeps_its_revision() {
    let model = from_json(TWO_NODES).unwrap().model;
    assert_eq!(model.clone().revision, model.revision);
}

#[test]
fn applying_a_change_moves_the_revision() {
    let mut model = from_json(TWO_NODES).unwrap().model;
    let before = model.revision;
    apply_batch(&mut model, &[add_node("c", "C")]);
    assert_ne!(
        model.revision, before,
        "a new node must invalidate a cached render"
    );

    let after_add = model.revision;
    apply_batch(&mut model, &[add_node("c", "Renamed")]);
    assert_ne!(
        model.revision, after_add,
        "a label update changes what is drawn, so it must move the revision too"
    );
}

#[test]
fn a_batch_that_changes_nothing_keeps_the_revision() {
    let mut model = from_json(TWO_NODES).unwrap().model;
    let before = model.revision;
    apply_batch(&mut model, &[]);
    apply_batch(
        &mut model,
        &[GraphEvent::RemoveNode {
            id: "missing".to_string(),
        }],
    );
    assert_eq!(model.revision, before);
}
