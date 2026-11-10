// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The schemas of the secret types, as a product's configuration reference
//! reads them.

use ferrohealth_secret::secret::Secret;
use ferrohealth_secret::secret_url::SecretUrl;
use serde_json::{Value, json};

/// The keywords that would carry or suggest a value.
const VALUE_KEYWORDS: [&str; 5] = ["default", "examples", "const", "enum", "pattern"];

fn assert_no_value(schema: &Value) {
    for keyword in VALUE_KEYWORDS {
        assert!(
            schema.get(keyword).is_none(),
            "the schema carries `{keyword}`"
        );
    }
}

#[test]
fn a_secret_is_a_write_only_string_read_inline_or_from_its_file() {
    let schema = serde_json::to_value(schemars::schema_for!(Secret)).expect("a schema serializes");
    assert_eq!(schema["type"], json!("string"));
    assert_eq!(schema["writeOnly"], json!(true));
    let description = schema["description"].as_str().expect("a description");
    assert!(description.contains("`<key>_file`"), "{description}");
    assert!(description.contains("`***`"), "{description}");
    assert_no_value(&schema);
}

#[test]
fn a_secret_url_is_a_string_whose_credentials_are_named_secret() {
    let schema =
        serde_json::to_value(schemars::schema_for!(SecretUrl)).expect("a schema serializes");
    assert_eq!(schema["type"], json!("string"));
    assert!(
        schema.get("writeOnly").is_none(),
        "the URL itself comes back, masked"
    );
    let description = schema["description"].as_str().expect("a description");
    assert!(description.contains("userinfo"), "{description}");
    assert!(description.contains("`<key>_file`"), "{description}");
    assert_no_value(&schema);
}

#[test]
fn a_section_refers_to_each_secret_by_its_name() {
    #[derive(schemars::JsonSchema)]
    #[expect(dead_code, reason = "only the schema of the section is read")]
    struct Section {
        password: Option<Secret>,
        url: SecretUrl,
    }
    let schema = serde_json::to_value(schemars::schema_for!(Section)).expect("a schema serializes");
    assert_eq!(
        schema["properties"]["url"],
        json!({ "$ref": "#/$defs/SecretUrl" })
    );
    assert_eq!(schema["$defs"]["Secret"]["writeOnly"], json!(true));
    assert_eq!(schema["required"], json!(["url"]));
}
