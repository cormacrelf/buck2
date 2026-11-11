/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! Parsing a YAML file loaded with `load()` into the same shape a JSON file
//! would have.

use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error as _;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;

/// Parse one YAML document into a JSON value.
///
/// YAML 1.2 core schema: `yes`/`no`/`on`/`off` are strings, not booleans.
/// Mapping keys must be strings; a key that YAML resolves to anything else
/// (`1:`, `true:`, `null:`, a sequence) is an error, since a Starlark value
/// loaded from data has string keys only, as JSON does. Quote such a key to
/// use it.
pub(crate) fn yaml_to_json(contents: &str) -> buck2_error::Result<serde_json::Value> {
    // The default budget is sized for configuration files. Lockfiles such as
    // `pnpm-lock.yaml` run to many times its node and event limits; the depth
    // and alias limits stay.
    let mut budget = serde_saphyr::Budget::default();
    budget.max_events = usize::MAX;
    budget.max_nodes = usize::MAX;
    budget.max_total_scalar_bytes = usize::MAX;
    let options = serde_saphyr::options! {
        budget: Some(budget),
        strict_booleans: true,
    };
    let JsonFromYaml(value) = serde_saphyr::from_str_with_options(contents, options)
        .map_err(|e| buck2_error::buck2_error!(buck2_error::ErrorTag::Input, "{e}"))?;
    Ok(value)
}

struct JsonFromYaml(serde_json::Value);

impl<'de> Deserialize<'de> for JsonFromYaml {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor).map(JsonFromYaml)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a YAML value representable as JSON")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(v.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(v)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom(format_args!("non-finite float `{v}` has no JSON form")))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(JsonFromYaml(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(serde_json::Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut object = serde_json::Map::new();
        while let Some(StringKey(key)) = map.next_key()? {
            let JsonFromYaml(value) = map.next_value()?;
            if object.insert(key.clone(), value).is_some() {
                return Err(A::Error::custom(format_args!(
                    "duplicate mapping key `{key}`"
                )));
            }
        }
        Ok(serde_json::Value::Object(object))
    }
}

/// A mapping key, which must be a YAML string. Asking for a `String` outright
/// would let the deserializer hand back the text of `1` or `true`, quietly
/// turning a number or boolean key into a string one.
struct StringKey(String);

impl<'de> Deserialize<'de> for StringKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_any(StringKeyVisitor)
            .map(StringKey)
    }
}

struct StringKeyVisitor;

impl<'de> Visitor<'de> for StringKeyVisitor {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string mapping key (quote the key to make it one)")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(v.to_owned())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::yaml_to_json;

    #[test]
    fn maps_like_json() {
        let yaml = r#"
lockfileVersion: '9.0'
settings:
  autoInstallPeers: true
  count: 3
  ratio: 0.5
  nothing: null
importers:
  .:
    dependencies:
      left-pad:
        specifier: ^1.3.0
        version: 1.3.0
packages:
  '@babel/core@7.24.0':
    resolution: {integrity: sha512-AAAA}
    engines: [node >=6.9.0]
"#;
        assert_eq!(
            json!({
                "lockfileVersion": "9.0",
                "settings": {
                    "autoInstallPeers": true,
                    "count": 3,
                    "ratio": 0.5,
                    "nothing": null,
                },
                "importers": {
                    ".": {
                        "dependencies": {
                            "left-pad": {"specifier": "^1.3.0", "version": "1.3.0"},
                        },
                    },
                },
                "packages": {
                    "@babel/core@7.24.0": {
                        "resolution": {"integrity": "sha512-AAAA"},
                        "engines": ["node >=6.9.0"],
                    },
                },
            }),
            yaml_to_json(yaml).unwrap()
        );
    }

    #[test]
    fn yaml_1_1_booleans_are_strings() {
        assert_eq!(
            json!({"country": "no", "flag": "yes", "real": false}),
            yaml_to_json("country: no\nflag: yes\nreal: false\n").unwrap()
        );
    }

    #[test]
    fn anchors_and_merge_keys_expand() {
        assert_eq!(
            json!({"base": {"a": 1}, "derived": {"a": 1, "b": 2}}),
            yaml_to_json("base: &b {a: 1}\nderived:\n  <<: *b\n  b: 2\n").unwrap()
        );
    }

    #[test]
    fn non_string_keys_are_rejected() {
        for yaml in [
            "1: a\n",
            "true: a\n",
            "null: a\n",
            "? [a, b]\n: c\n",
            "1.5: a\n",
        ] {
            let e = yaml_to_json(yaml).expect_err(yaml);
            assert!(
                format!("{e:#}").contains("string mapping key"),
                "`{yaml}` gave `{e:#}`"
            );
        }
        // Quoted, the same keys are strings.
        assert_eq!(
            json!({"1": "a", "true": "b"}),
            yaml_to_json("'1': a\n\"true\": b\n").unwrap()
        );
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        assert!(yaml_to_json("a: 1\na: 2\n").is_err());
    }

    #[test]
    fn several_documents_are_rejected() {
        assert!(yaml_to_json("a: 1\n---\nb: 2\n").is_err());
    }
}
