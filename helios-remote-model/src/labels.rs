use std::collections::HashMap;
use std::ops::Deref;

use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
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
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Labels {
            List(Vec<String>),
            Map(HashMap<String, String>),
        }

        let labels = match Labels::deserialize(deserializer)? {
            Labels::List(labels) => labels
                .into_iter()
                .map(|label| {
                    // look for the first `=`, assume the value is an empty string
                    // if none
                    let parts: Vec<&str> = label.splitn(2, '=').collect();
                    let key = parts[0].to_string();
                    let value = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
                    (key, value)
                })
                .collect(),
            Labels::Map(labels) => labels,
        };

        Ok(Self(labels))
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
