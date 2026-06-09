use std::fmt;

use mahler::json::Operation;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields, format::Writer},
    registry::{LookupSpan, Scope},
};

static SENSITIVE_KEYWORDS: &[&str] = &[
    "auth", "card", "cert", "cookie", "cred", "cvv", "cvc", "e-mail", "e_mail", "email", "key",
    "pass", "phone", "pin", "private", "secret", "session", "ssn", "token", "bearer", "salt",
    "hash",
];

fn mask_uri_password(value: &mut serde_json::Value) {
    let Some(s) = value.as_str() else { return };
    let Ok(uri) = s.parse::<axum::http::Uri>() else {
        return;
    };
    let Some(authority) = uri.authority() else {
        return;
    };
    let authority_str = authority.as_str();
    let Some(at_pos) = authority_str.find('@') else {
        return;
    };
    let userinfo = &authority_str[..at_pos];
    let Some(colon_pos) = userinfo.find(':') else {
        return;
    };
    let user = &userinfo[..colon_pos];
    let host = &authority_str[at_pos + 1..];
    let scheme = uri.scheme_str().unwrap_or_default();
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_default();
    let masked = format!("{scheme}://{user}:***@{host}{path_and_query}");
    *value = serde_json::Value::String(masked);
}

fn mask_if_sensitive(key: &str, value: &mut serde_json::Value) {
    let lower = key.to_lowercase();
    if SENSITIVE_KEYWORDS.iter().any(|word| lower.contains(word)) {
        *value = serde_json::Value::String("***".to_string());
    } else {
        mask_uri_password(value);
    }
}

fn mask_object(key: &str, value: &mut serde_json::Value) {
    if key == "environment"
        && let Some(env_vars) = value.as_object_mut()
    {
        for (key, value) in env_vars {
            mask_if_sensitive(key, value)
        }
    } else if let Some(obj) = value.as_object_mut() {
        for (key, value) in obj {
            mask_object(key, value)
        }
    }
}

pub fn mask_sensitive_data(mut operation: Operation) -> Operation {
    let (path, value) = match operation {
        Operation::Create {
            ref mut value,
            ref path,
        } => (path, value),
        Operation::Update {
            ref mut value,
            ref path,
        } => (path, value),
        Operation::Delete { .. } => return operation,
    };

    let pointer = path.as_ref();
    let last = pointer.last();
    let parent_token = pointer.parent().and_then(|p| p.last());

    // if the component parent is "environment", then mask the value if sensitive
    if let Some(key) = last.as_ref()
        && let Some(parent) = parent_token
        && parent.encoded() == "environment"
    {
        mask_if_sensitive(key.encoded(), value);
    } else if let Some(var_name) = last.as_ref() {
        mask_object(var_name.encoded(), value)
    }

    operation
}

/// A custom event formatter that allows to configure the outermost
/// span in the scope via the display_name property.
///
/// Events will read a `<display_name>:poll:..` rather than using the crate-derived target.
pub struct LocalEventFormatter {
    pub display_name: String,
}

impl LocalEventFormatter {
    /// Right-padded, optionally coloured level tag, matching the built-in
    /// compact formatter.
    fn write_level(&self, writer: &mut Writer<'_>, level: &Level) -> fmt::Result {
        let (tag, color) = match *level {
            Level::TRACE => ("TRACE", "\x1b[35m"),
            Level::DEBUG => ("DEBUG", "\x1b[34m"),
            Level::INFO => (" INFO", "\x1b[32m"),
            Level::WARN => (" WARN", "\x1b[33m"),
            Level::ERROR => ("ERROR", "\x1b[31m"),
        };
        if writer.has_ansi_escapes() {
            write!(writer, "{color}{tag}\x1b[0m ")
        } else {
            write!(writer, "{tag} ")
        }
    }
}

impl<S, N> FormatEvent<S, N> for LocalEventFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let ansi = writer.has_ansi_escapes();

        self.write_level(&mut writer, meta.level())?;

        // Service name first, then each span in the scope from the root down,
        // joined by ':' (e.g. `helios:poll:`).
        let bold = |name: &str| -> String {
            if ansi {
                format!("\x1b[1m{name}\x1b[0m:")
            } else {
                format!("{name}:")
            }
        };
        write!(writer, "{}", bold(&self.display_name))?;
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                write!(writer, "{}", bold(span.name()))?;
            }
        }
        writer.write_char(' ')?;

        // Event fields (renders the `message` field as the log line).
        ctx.format_fields(writer.by_ref(), event)?;

        // Append span fields dimmed, matching the compact formatter.
        for span in ctx.event_scope().into_iter().flat_map(Scope::from_root) {
            let exts = span.extensions();
            if let Some(fields) = exts.get::<FormattedFields<N>>()
                && !fields.is_empty()
            {
                if ansi {
                    write!(writer, " \x1b[2m{}\x1b[0m", fields.fields)?;
                } else {
                    write!(writer, " {}", fields.fields)?;
                }
            }
        }
        writeln!(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mahler::json::{Path, Value};
    use serde_json::json;

    fn create_op(path: &'static str, value: Value) -> Operation {
        Operation::Create {
            path: Path::from_static(path),
            value,
        }
    }

    fn update_op(path: &'static str, value: Value) -> Operation {
        Operation::Update {
            path: Path::from_static(path),
            value,
        }
    }

    #[test]
    fn masks_token_in_environment_object() {
        let op = create_op(
            "/services/myapp",
            json!({
                "image": "registry.io/app:latest",
                "environment": {
                    "API_TOKEN": "secret123",
                    "DATABASE_PASSWORD": "hunter2",
                    "APP_NAME": "myapp"
                }
            }),
        );
        let result = mask_sensitive_data(op);
        let Operation::Create { value, .. } = result else {
            panic!("expected Create");
        };
        assert_eq!(value["environment"]["API_TOKEN"], "***");
        assert_eq!(value["environment"]["DATABASE_PASSWORD"], "***");
        assert_eq!(value["environment"]["APP_NAME"], "myapp");
    }

    #[test]
    fn masks_direct_environment_path_value() {
        let op = create_op("/services/myapp/environment/SECRET_KEY", json!("s3cr3t"));
        let result = mask_sensitive_data(op);
        let Operation::Create { value, .. } = result else {
            panic!("expected Create");
        };
        assert_eq!(value, json!("***"));
    }

    #[test]
    fn does_not_mask_non_sensitive_direct_env_var() {
        let op = create_op("/services/myapp/environment/PORT", json!("8080"));
        let result = mask_sensitive_data(op);
        let Operation::Create { value, .. } = result else {
            panic!("expected Create");
        };
        assert_eq!(value, json!("8080"));
    }

    #[test]
    fn masks_nested_environment_in_deep_object() {
        let op = update_op(
            "/services",
            json!({
                "my-service": {
                    "environment": {
                        "AUTH_HEADER": "Bearer xyz",
                        "LOG_LEVEL": "debug"
                    }
                }
            }),
        );
        let result = mask_sensitive_data(op);
        let Operation::Update { value, .. } = result else {
            panic!("expected Update");
        };
        assert_eq!(value["my-service"]["environment"]["AUTH_HEADER"], "***");
        assert_eq!(value["my-service"]["environment"]["LOG_LEVEL"], "debug");
    }

    #[test]
    fn delete_operations_pass_through() {
        let op = Operation::Delete {
            path: Path::from_static("/services/myapp/environment/SECRET_KEY"),
        };
        let result = mask_sensitive_data(op.clone());
        assert_eq!(result, op);
    }

    #[test]
    fn masks_all_sensitive_keywords() {
        for keyword in SENSITIVE_KEYWORDS {
            let var_name = format!("MY_{}", keyword.to_uppercase());
            let path = format!("/services/my-svc/environment/{var_name}");
            // Use Path::new via the PatchOperation conversion to handle dynamic paths
            let op = Operation::Create {
                path: Path::from_static(Box::leak(path.into_boxed_str())),
                value: json!("sensitive_value"),
            };
            let result = mask_sensitive_data(op);
            let Operation::Create { value, .. } = result else {
                panic!("expected Create");
            };
            assert_eq!(value, json!("***"), "keyword '{keyword}' should be masked");
        }
    }

    #[test]
    fn case_insensitive_masking() {
        let op = create_op(
            "/services/my-svc",
            json!({
                "environment": {
                    "Api_Token": "val1",
                    "DATABASE_PASSWORD": "val2",
                    "my_secret_var": "val3"
                }
            }),
        );
        let result = mask_sensitive_data(op);
        let Operation::Create { value, .. } = result else {
            panic!("expected Create");
        };
        assert_eq!(value["environment"]["Api_Token"], "***");
        assert_eq!(value["environment"]["DATABASE_PASSWORD"], "***");
        assert_eq!(value["environment"]["my_secret_var"], "***");
    }

    #[test]
    fn masks_uri_password_in_environment_value() {
        let op = create_op(
            "/services/myapp",
            json!({
                "environment": {
                    "DATABASE_URL": "postgres://admin:s3cret@db.example.com:5432/mydb",
                    "REDIS_URL": "redis://user:hunter2@redis.local/0",
                    "API_ENDPOINT": "https://api.example.com/v1",
                    "PLAIN_VAR": "no-uri-here"
                }
            }),
        );
        let result = mask_sensitive_data(op);
        let Operation::Create { value, .. } = result else {
            panic!("expected Create");
        };
        assert_eq!(
            value["environment"]["DATABASE_URL"],
            "postgres://admin:***@db.example.com:5432/mydb"
        );
        assert_eq!(
            value["environment"]["REDIS_URL"],
            "redis://user:***@redis.local/0"
        );
        assert_eq!(
            value["environment"]["API_ENDPOINT"],
            "https://api.example.com/v1"
        );
        assert_eq!(value["environment"]["PLAIN_VAR"], "no-uri-here");
    }

    #[test]
    fn masks_uri_password_in_direct_env_path() {
        let op = create_op(
            "/services/myapp/environment/DATABASE_URL",
            json!("postgres://admin:s3cret@db.example.com:5432/mydb"),
        );
        let result = mask_sensitive_data(op);
        let Operation::Create { value, .. } = result else {
            panic!("expected Create");
        };
        assert_eq!(
            value,
            json!("postgres://admin:***@db.example.com:5432/mydb")
        );
    }

    #[test]
    fn does_not_mask_non_environment_objects() {
        let op = create_op(
            "/services/app",
            json!({
                "labels": {
                    "API_TOKEN": "not_masked"
                }
            }),
        );
        let result = mask_sensitive_data(op);
        let Operation::Create { value, .. } = result else {
            panic!("expected Create");
        };
        assert_eq!(value["labels"]["API_TOKEN"], "not_masked");
    }
}
