// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The schema of the `[usage_report]` section, as a product's configuration
//! reference reads it.

use ferrohealth_usage_report::section::{ENDPOINT, UsageReportSection};
use serde_json::json;

#[test]
fn the_section_schema_carries_each_key_with_its_doc_and_default() {
    let schema = serde_json::to_value(schemars::schema_for!(UsageReportSection))
        .expect("a schema serializes");
    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["additionalProperties"], json!(false));
    let properties = schema["properties"].as_object().expect("the keys");
    let mut keys = properties.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["deployment", "enabled", "endpoint", "state_dir"]);
    for (key, property) in properties {
        assert!(
            property["description"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "`{key}` carries no description"
        );
    }
    assert_eq!(properties["enabled"]["default"], json!(true));
    assert_eq!(properties["endpoint"]["default"], json!(ENDPOINT));
    assert_eq!(properties["deployment"]["default"], json!("unknown"));

    let endpoint = &schema["$defs"]["Endpoint"];
    assert_eq!(endpoint["type"], json!("string"));
    assert_eq!(endpoint["format"], json!("uri"));
    let deployment = serde_json::to_string(&schema["$defs"]["Deployment"]).expect("JSON");
    for name in ["helm", "compose", "binary", "unknown"] {
        assert!(deployment.contains(&format!("\"{name}\"")), "{deployment}");
    }
}
