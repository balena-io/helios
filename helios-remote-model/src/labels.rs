use std::collections::HashMap;
use std::ops::Deref;

use serde::{Deserialize, Deserializer};

#[derive(Debug, PartialEq, Eq, Default)]
pub struct Labels(HashMap<String, String>);

impl Deref for Labels {
    type Target = HashMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Labels> for HashMap<String, String> {
    fn from(value: Labels) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for Labels {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        struct LabelsVisitor;

        impl<'de> de::Visitor<'de> for LabelsVisitor {
            type Value = Labels;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a list of \"key=value\" strings or a map of names to string values")
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut labels = HashMap::new();
                while let Some(label) = seq.next_element::<String>()? {
                    // look for the first `=`, assume the value is an empty string
                    // if none
                    let (key, value) = match label.split_once('=') {
                        Some((k, v)) => (k.to_string(), v.to_string()),
                        None => (label, String::new()),
                    };
                    labels.insert(key, value);
                }
                Ok(Labels(labels))
            }

            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut labels = HashMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value: String = map.next_value()?;
                    labels.insert(key, value);
                }
                Ok(Labels(labels))
            }
        }

        deserializer.deserialize_any(LabelsVisitor)
    }
}

impl IntoIterator for Labels {
    type Item = (String, String);
    type IntoIter = std::collections::hash_map::IntoIter<String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labels_from_map() {
        let labels: Labels = serde_json::from_value(json!({
            "key1": "value1",
            "key2": "value2"
        }))
        .unwrap();

        assert_eq!(labels.get("key1"), Some(&"value1".to_string()));
        assert_eq!(labels.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn labels_from_list() {
        let labels: Labels = serde_json::from_value(json!(["key1=value1", "key2=value2"])).unwrap();

        assert_eq!(labels.get("key1"), Some(&"value1".to_string()));
        assert_eq!(labels.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn labels_from_list_without_value() {
        let labels: Labels = serde_json::from_value(json!(["key1", "key2=value2"])).unwrap();

        assert_eq!(labels.get("key1"), Some(&"".to_string()));
        assert_eq!(labels.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn labels_from_list_with_equals_in_value() {
        let labels: Labels = serde_json::from_value(json!(["key1=a=b=c"])).unwrap();

        assert_eq!(labels.get("key1"), Some(&"a=b=c".to_string()));
    }

    #[test]
    fn labels_from_empty_map() {
        let labels: Labels = serde_json::from_value(json!({})).unwrap();
        assert!(labels.is_empty());
    }

    #[test]
    fn labels_from_empty_list() {
        let labels: Labels = serde_json::from_value(json!([])).unwrap();
        assert!(labels.is_empty());
    }

    #[test]
    fn labels_into_hashmap() {
        let labels: Labels = serde_json::from_value(json!({"key": "value"})).unwrap();
        let map: HashMap<String, String> = labels.into();
        assert_eq!(map.get("key"), Some(&"value".to_string()));
    }
}
