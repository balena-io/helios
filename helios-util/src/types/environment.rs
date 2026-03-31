use std::collections::HashMap;
use std::convert::Infallible;
use std::ops::{Deref, DerefMut};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    String(String),
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        struct ValueVisitor;

        impl<'de> de::Visitor<'de> for ValueVisitor {
            type Value = Value;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a boolean, number, or string")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
                Ok(Value::Bool(v))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
                Ok(Value::Unsigned(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
                Ok(Value::Signed(v))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
                Ok(Value::Float(v))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
                // Parse the string to get the typed value
                Ok(v.parse().unwrap())
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

impl Eq for Value {}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Unsigned(u) => write!(f, "{u}"),
            Value::Signed(i) => write!(f, "{i}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::String(s) => write!(f, "{s}"),
        }
    }
}

impl FromStr for Value {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(b) = s.parse::<bool>() {
            return Ok(Value::Bool(b));
        }
        if let Ok(u) = s.parse::<u64>() {
            return Ok(Value::Unsigned(u));
        }
        if let Ok(i) = s.parse::<i64>() {
            return Ok(Value::Signed(i));
        }
        if let Ok(f) = s.parse::<f64>() {
            return Ok(Value::Float(f));
        }
        Ok(Value::String(s.to_string()))
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        s.parse().unwrap()
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        s.parse().unwrap()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
pub struct Environment(HashMap<String, Option<Value>>);

impl Deref for Environment {
    type Target = HashMap<String, Option<Value>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Environment {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Environment {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Environment> for HashMap<String, Option<Value>> {
    fn from(value: Environment) -> Self {
        value.0
    }
}

impl FromIterator<(String, Option<Value>)> for Environment {
    fn from_iter<I: IntoIterator<Item = (String, Option<Value>)>>(iter: I) -> Self {
        Environment(HashMap::from_iter(iter))
    }
}

impl<V: Into<Value>> FromIterator<(String, V)> for Environment {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let map: HashMap<String, V> = HashMap::from_iter(iter);
        Environment(map.into_iter().map(|(k, v)| (k, Some(v.into()))).collect())
    }
}

impl IntoIterator for Environment {
    type Item = (String, Option<Value>);
    type IntoIter = std::collections::hash_map::IntoIter<String, Option<Value>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Environment {
    type Item = (&'a String, &'a Option<Value>);
    type IntoIter = std::collections::hash_map::Iter<'a, String, Option<Value>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut Environment {
    type Item = (&'a String, &'a mut Option<Value>);
    type IntoIter = std::collections::hash_map::IterMut<'a, String, Option<Value>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl From<Vec<String>> for Environment {
    fn from(value: Vec<String>) -> Self {
        let mut map = HashMap::with_capacity(value.len());
        for entry in value {
            let (key, value) = match entry.split_once('=') {
                // the unwrap here is OK since the parse is infallible
                Some((k, v)) => (k, Some(v.parse().unwrap())),
                None => (entry.as_str(), None),
            };
            map.insert(key.to_string(), value);
        }
        Self(map)
    }
}

impl<'de> Deserialize<'de> for Environment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Env {
            List(Vec<String>),
            Map(HashMap<String, Option<Value>>),
        }

        match Env::deserialize(deserializer)? {
            Env::List(env_vars) => Ok(Environment::from(env_vars)),
            Env::Map(env) => Ok(Self(env)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deserialize_map_with_typed_values() {
        let json = json!({
            "DEBUG": true,
            "PORT": 8080,
            "HOST": "localhost",
            "RATIO": 1.5
        });

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(env.get("DEBUG"), Some(&Some(Value::Bool(true))));
        assert_eq!(env.get("PORT"), Some(&Some(Value::Unsigned(8080))));
        assert_eq!(
            env.get("HOST"),
            Some(&Some(Value::String("localhost".to_string())))
        );
        assert_eq!(env.get("RATIO"), Some(&Some(Value::Float(1.5))));
    }

    #[test]
    fn test_deserialize_map_with_string_values() {
        let json = json!({
            "KEY": "value",
            "EMPTY": ""
        });

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(
            env.get("KEY"),
            Some(&Some(Value::String("value".to_string())))
        );
        assert_eq!(env.get("EMPTY"), Some(&Some(Value::String("".to_string()))));
    }

    #[test]
    fn test_deserialize_map_with_null_value() {
        let json = json!({
            "UNSET": null,
            "SET": "value"
        });

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(env.get("UNSET"), Some(&None));
        assert_eq!(
            env.get("SET"),
            Some(&Some(Value::String("value".to_string())))
        );
    }

    #[test]
    fn test_deserialize_list_parses_values() {
        let json = json!([
            "PORT=8080",
            "DEBUG=true",
            "HOST=localhost",
            "TEMP=-10",
            "RATIO=2.14"
        ]);

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(env.get("PORT"), Some(&Some(Value::Unsigned(8080))));
        assert_eq!(env.get("DEBUG"), Some(&Some(Value::Bool(true))));
        assert_eq!(
            env.get("HOST"),
            Some(&Some(Value::String("localhost".to_string())))
        );
        assert_eq!(env.get("TEMP"), Some(&Some(Value::Signed(-10))));
        assert_eq!(env.get("RATIO"), Some(&Some(Value::Float(2.14))));
    }

    #[test]
    fn test_deserialize_list_with_empty_value() {
        let json = json!(["KEY="]);

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(env.get("KEY"), Some(&Some(Value::String("".to_string()))));
    }

    #[test]
    fn test_deserialize_list_with_equals_in_value() {
        let json = json!(["FORMULA=a=b+c"]);

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(
            env.get("FORMULA"),
            Some(&Some(Value::String("a=b+c".to_string())))
        );
    }

    #[test]
    fn test_deserialize_list_without_value() {
        let json = json!(["NO_VALUE"]);

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(env.get("NO_VALUE"), Some(&None));
    }

    #[test]
    fn test_deserialize_list_allows_empty_key() {
        let json = json!(["=value"]);

        let env: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(env.get(""), Some(&Some(Value::String("value".to_string()))));
    }

    #[test]
    fn test_deserialize_empty_map() {
        let json = json!({});

        let env: Environment = serde_json::from_value(json).unwrap();
        assert!(env.is_empty());
    }

    #[test]
    fn test_deserialize_empty_list() {
        let json = json!([]);

        let env: Environment = serde_json::from_value(json).unwrap();
        assert!(env.is_empty());
    }
}
