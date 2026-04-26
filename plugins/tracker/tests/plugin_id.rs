use tracker::PLUGIN_ID;

#[test]
fn plugin_json_name_matches_plugin_id_constant() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../plugin.json")).expect("plugin.json is valid JSON");
    let name = manifest["name"]
        .as_str()
        .expect("plugin.json must have a string 'name' field");
    assert_eq!(
        name,
        PLUGIN_ID,
        "plugin.json 'name' ({name:?}) must equal PLUGIN_ID ({PLUGIN_ID:?})"
    );
}
