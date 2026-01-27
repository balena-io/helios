use serde::Deserialize;
use serde::de::{self, Deserializer};
use std::ops::Deref;

#[derive(Clone, Debug)]
pub struct Command(Vec<String>);

impl Deref for Command {
    type Target = Vec<String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Command {
            String(String),
            List(Vec<String>),
        }

        let command = match Command::deserialize(deserializer)? {
            Command::String(cmd) => shell_words::split(&cmd).map_err(de::Error::custom)?,
            Command::List(cmd) => cmd,
        };

        Ok(Self(command))
    }
}

impl IntoIterator for Command {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_from_simple_string() {
        let cmd: Command = serde_json::from_value(json!("echo hello")).unwrap();
        assert_eq!(cmd.0, vec!["echo", "hello"]);
    }

    #[test]
    fn command_from_single_quoted_string() {
        let cmd: Command = serde_json::from_value(json!("echo 'hello world'")).unwrap();
        assert_eq!(cmd.0, vec!["echo", "hello world"]);
    }

    #[test]
    fn command_from_double_quoted_string() {
        let cmd: Command = serde_json::from_value(json!("echo \"hello world\"")).unwrap();
        assert_eq!(cmd.0, vec!["echo", "hello world"]);
    }

    #[test]
    fn command_from_shell_invocation() {
        let cmd: Command = serde_json::from_value(json!("sh -c \"echo 'hello world'\"")).unwrap();
        assert_eq!(cmd.0, vec!["sh", "-c", "echo 'hello world'"]);
    }

    #[test]
    fn command_preserves_shell_variables() {
        let cmd: Command = serde_json::from_value(json!("echo $VAR")).unwrap();
        assert_eq!(cmd.0, vec!["echo", "$VAR"]);
    }

    #[test]
    fn command_from_vec() {
        let cmd: Command = serde_json::from_value(json!(["echo", "hello"])).unwrap();
        assert_eq!(cmd.0, vec!["echo", "hello"]);
    }

    #[test]
    fn command_from_mixed_quotes() {
        let cmd: Command = serde_json::from_value(json!("cmd 'arg one' \"arg two\"")).unwrap();
        assert_eq!(cmd.0, vec!["cmd", "arg one", "arg two"]);
    }

    #[test]
    fn command_invalid_unclosed_quote() {
        let result: Result<Command, _> = serde_json::from_value(json!("echo 'hello"));
        assert!(result.is_err());
    }
}
