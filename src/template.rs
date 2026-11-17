//! `${VAR}` expansion for `config.yaml` and `log.yaml`, in the style of
//! docker-compose.
//!
//! Values come from the environment, with `.env` filling the gaps — the same
//! rule the credentials already follow. That lets the configuration files be
//! committed unchanged while the machine-specific half lives in `.env` or in
//! the service manager's environment.
//!
//! Supported forms:
//!
//! | Syntax | Meaning |
//! | --- | --- |
//! | `${NAME}` | value of `NAME`; **error** when it is not set |
//! | `${NAME:-fallback}` | value of `NAME`, or `fallback` when unset/empty |
//! | `${NAME:?message}` | value of `NAME`, or an error carrying `message` |
//! | `$$` | a literal `$` |
//!
//! Expansion is a single pass over the raw text, so a value that itself
//! contains `$` is inserted verbatim and never re-expanded.

use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq)]
pub enum ExpandError {
    /// `${NAME}` with no value anywhere.
    Undefined { name: String, at: String },
    /// `${NAME:?message}` and the value was missing.
    Required {
        name: String,
        message: String,
        at: String,
    },
    /// `${` without a closing brace.
    Unterminated { at: String },
    /// `${1BAD}` and friends.
    InvalidName { name: String, at: String },
}

impl ExpandError {
    /// One line, ready to be prefixed with the file name.
    pub fn message(&self) -> String {
        match self {
            ExpandError::Undefined { name, at } => format!(
                "{at}: variable {name} is not set (use ${{{name}:-default}} to supply a default)"
            ),
            ExpandError::Required { name, message, at } => {
                if message.is_empty() {
                    format!("{at}: variable {name} is not set")
                } else {
                    format!("{at}: {message} ({name})")
                }
            }
            ExpandError::Unterminated { at } => {
                format!("{at}: `${{` without a closing `}}`")
            }
            ExpandError::InvalidName { name, at } => {
                format!("{at}: invalid variable name `{name}`")
            }
        }
    }

    /// Replaces where the problem was found: a line while scanning raw text, a
    /// configuration path (`targets[0].url`) when walking parsed YAML.
    fn relocate(self, location: String) -> Self {
        match self {
            ExpandError::Undefined { name, .. } => ExpandError::Undefined { name, at: location },
            ExpandError::Required { name, message, .. } => ExpandError::Required {
                name,
                message,
                at: location,
            },
            ExpandError::Unterminated { .. } => ExpandError::Unterminated { at: location },
            ExpandError::InvalidName { name, .. } => {
                ExpandError::InvalidName { name, at: location }
            }
        }
    }
}

enum Mode {
    Plain,
    Default(String),
    Required(String),
}

/// Replaces every `${…}` placeholder using `vars`.
pub fn expand(input: &str, vars: &HashMap<String, String>) -> Result<String, ExpandError> {
    let mut out = String::with_capacity(input.len());
    let mut line = 1usize;
    let mut chars = input.chars();

    while let Some(current) = chars.next() {
        if current == '\n' {
            line += 1;
            out.push(current);
            continue;
        }
        if current != '$' {
            out.push(current);
            continue;
        }

        // `$$` is an escaped dollar sign.
        if chars.clone().next() == Some('$') {
            chars.next();
            out.push('$');
            continue;
        }

        if chars.clone().next() != Some('{') {
            out.push('$');
            continue;
        }
        chars.next(); // the opening brace

        let mut body = String::new();
        let mut closed = false;
        for ch in chars.by_ref() {
            if ch == '}' {
                closed = true;
                break;
            }
            body.push(ch);
        }
        if !closed {
            return Err(ExpandError::Unterminated {
                at: format!("line {line}"),
            });
        }

        let (name, mode) = split_operator(&body);
        if !valid_name(&name) {
            return Err(ExpandError::InvalidName {
                name,
                at: format!("line {line}"),
            });
        }

        match mode {
            Mode::Plain => match vars.get(&name) {
                Some(value) => out.push_str(value),
                None => {
                    return Err(ExpandError::Undefined {
                        name,
                        at: format!("line {line}"),
                    })
                }
            },
            Mode::Default(fallback) => {
                let value = vars
                    .get(&name)
                    .filter(|value| !value.is_empty())
                    .cloned()
                    .unwrap_or(fallback);
                out.push_str(&value);
            }
            Mode::Required(message) => match vars.get(&name).filter(|value| !value.is_empty()) {
                Some(value) => out.push_str(value),
                None => {
                    return Err(ExpandError::Required {
                        name,
                        message,
                        at: format!("line {line}"),
                    })
                }
            },
        }
    }

    Ok(out)
}

/// Expands every string **value** in an already parsed YAML/JSON document.
///
/// Working on the parsed document rather than on the raw text is what keeps
/// documentation comments like ``Supported: ${VAR}, ${VAR:-default}`` from being
/// treated as real placeholders — the YAML parser has already dropped comments
/// by the time this runs. Object keys are left untouched.
pub fn expand_value(
    value: &mut serde_json::Value,
    vars: &HashMap<String, String>,
) -> Result<(), ExpandError> {
    walk(value, "", vars)
}

fn walk(
    value: &mut serde_json::Value,
    path: &str,
    vars: &HashMap<String, String>,
) -> Result<(), ExpandError> {
    match value {
        serde_json::Value::String(text) => {
            let expanded = expand(text, vars).map_err(|err| {
                err.relocate(if path.is_empty() {
                    "value".to_string()
                } else {
                    path.to_string()
                })
            })?;
            *text = expanded;
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                walk(item, &format!("{path}[{index}]"), vars)?;
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                walk(item, &child, vars)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn split_operator(body: &str) -> (String, Mode) {
    if let Some(index) = body.find(":-") {
        return (
            body[..index].trim().to_string(),
            Mode::Default(body[index + 2..].to_string()),
        );
    }
    if let Some(index) = body.find(":?") {
        return (
            body[..index].trim().to_string(),
            Mode::Required(body[index + 2..].trim().to_string()),
        );
    }
    (body.trim().to_string(), Mode::Plain)
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn replaces_a_plain_placeholder() {
        let text = "url: \"${NPM_SOURCE_URL}\"\n";
        let out = expand(
            text,
            &vars(&[("NPM_SOURCE_URL", "http://192.0.2.1:81/api")]),
        )
        .unwrap();
        assert_eq!(out, "url: \"http://192.0.2.1:81/api\"\n");
    }

    #[test]
    fn replaces_several_placeholders_in_one_line() {
        let text = "identity: \"${A}\" secret: \"${B}\"";
        let out = expand(text, &vars(&[("A", "user"), ("B", "pass")])).unwrap();
        assert_eq!(out, "identity: \"user\" secret: \"pass\"");
    }

    #[test]
    fn leaves_unrelated_text_alone() {
        let text = "no placeholders here: $host and $ 100%";
        assert_eq!(expand(text, &HashMap::new()).unwrap(), text);
    }

    #[test]
    fn dollar_dollar_is_a_literal_dollar() {
        let out = expand("cost: $$5 and ${A}", &vars(&[("A", "x")])).unwrap();
        assert_eq!(out, "cost: $5 and x");
    }

    #[test]
    fn default_is_used_when_missing_or_empty() {
        let text = "${PORT:-81}/${HOST:-localhost}/${EMPTY:-fallback}";
        let out = expand(text, &vars(&[("E MP TY", "")])).ok();
        // EMPTY is not defined at all here
        assert_eq!(out.unwrap(), "81/localhost/fallback");

        let out = expand("${PORT:-81}", &vars(&[("PORT", "")])).unwrap();
        assert_eq!(out, "81", "an empty value falls back too");
    }

    #[test]
    fn defined_value_beats_the_default() {
        let out = expand("${PORT:-81}", &vars(&[("PORT", "8080")])).unwrap();
        assert_eq!(out, "8080");
    }

    #[test]
    fn required_placeholder_reports_its_message() {
        let error = expand("${TOKEN:?set TOKEN in .env}", &HashMap::new()).unwrap_err();
        assert_eq!(
            error,
            ExpandError::Required {
                name: "TOKEN".into(),
                message: "set TOKEN in .env".into(),
                at: "line 1".to_string(),
            }
        );
        assert_eq!(error.message(), "line 1: set TOKEN in .env (TOKEN)");
    }

    #[test]
    fn undefined_placeholder_names_variable_and_line() {
        let error = expand("first: ok\nsecond: ${MISSING}\n", &HashMap::new()).unwrap_err();
        assert_eq!(
            error,
            ExpandError::Undefined {
                name: "MISSING".into(),
                at: "line 2".to_string(),
            }
        );
        assert!(error.message().contains("MISSING"));
        assert!(error.message().contains("line 2"));
    }

    #[test]
    fn unterminated_placeholder_is_an_error() {
        let error = expand("url: ${BROKEN\n", &HashMap::new()).unwrap_err();
        assert_eq!(
            error,
            ExpandError::Unterminated {
                at: "line 1".to_string()
            }
        );
    }

    #[test]
    fn invalid_names_are_rejected() {
        assert_eq!(
            expand("${1BAD}", &HashMap::new()).unwrap_err(),
            ExpandError::InvalidName {
                name: "1BAD".into(),
                at: "line 1".to_string()
            }
        );
        assert_eq!(
            expand("${}", &HashMap::new()).unwrap_err(),
            ExpandError::InvalidName {
                name: "".into(),
                at: "line 1".to_string()
            }
        );
    }

    #[test]
    fn expanded_values_are_not_re_expanded() {
        // A password that happens to look like a placeholder must stay as it is.
        let out = expand("${SECRET}", &vars(&[("SECRET", "${NOT_A_VAR}")])).unwrap();
        assert_eq!(out, "${NOT_A_VAR}");
    }
}
