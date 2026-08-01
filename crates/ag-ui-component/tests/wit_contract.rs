use std::path::PathBuf;

#[test]
fn extension_world_parses_and_exports_the_contract() {
    let wit_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit");
    let mut resolve = wit_parser::Resolve::default();
    let (package_id, _) = resolve
        .push_dir(&wit_dir)
        .expect("the checked-in WIT package must parse");
    let package = &resolve.packages[package_id];
    let world_id = package
        .worlds
        .get("extension-component")
        .copied()
        .expect("the extension-component world must exist");
    let world = &resolve.worlds[world_id];

    let export_names: Vec<String> = world
        .exports
        .keys()
        .filter_map(|key| match key {
            wit_parser::WorldKey::Name(name) => Some(name.clone()),
            wit_parser::WorldKey::Interface(interface_id) => {
                resolve.interfaces[*interface_id].name.clone()
            }
        })
        .collect();

    assert!(export_names.iter().any(|name| name == "types"));
    assert!(export_names.iter().any(|name| name == "extension"));
    assert!(world.imports.is_empty());

    let extension = world
        .exports
        .values()
        .find_map(|item| match item {
            wit_parser::WorldItem::Interface { id, .. }
                if resolve.interfaces[*id].name.as_deref() == Some("extension") =>
            {
                Some(&resolve.interfaces[*id])
            }
            _ => None,
        })
        .expect("the extension interface must be exported");
    for operation in [
        "describe",
        "invoke",
        "describe-state",
        "snapshot",
        "restore",
    ] {
        assert!(extension.functions.contains_key(operation));
    }
}
