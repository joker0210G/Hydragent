use std::collections::HashMap;
use crate::taint::TaintedString;

pub struct KeyInjector {
    vault_values: HashMap<String, TaintedString>,
}

impl KeyInjector {
    pub fn new(vault_values: HashMap<String, TaintedString>) -> Self {
        Self { vault_values }
    }

    /// Replaces placeholders like `{{SECRET_KEY}}` only if message role is "system" or "tool".
    pub fn inject_message(&self, role: &str, content: &str) -> (TaintedString, Vec<String>) {
        let mut injected_scopes = Vec::new();
        let mut result = content.to_string();

        if role == "system" || role == "tool" {
            for (scope, tainted_val) in &self.vault_values {
                let placeholder = format!("{{{{{}}}}}", scope);
                if result.contains(&placeholder) {
                    result = result.replace(&placeholder, tainted_val.expose_secret());
                    injected_scopes.push(scope.clone());
                }
            }
        }

        (TaintedString::new(result), injected_scopes)
    }
}

impl Drop for KeyInjector {
    fn drop(&mut self) {
        self.vault_values.clear();
    }
}


/// Inject secrets into a template string by replacing `{{SECRET_KEY}}` placeholders.
pub fn inject_str(template: &str, secrets: &HashMap<String, TaintedString>) -> String {
    let mut result = String::new();
    let mut current = template;
    while let Some(start_idx) = current.find("{{") {
        result.push_str(&current[..start_idx]);
        let remaining = &current[start_idx + 2..];
        if let Some(end_idx) = remaining.find("}}") {
            let key = remaining[..end_idx].trim();
            if let Some(secret) = secrets.get(key) {
                result.push_str(secret.expose_secret());
            } else {
                result.push_str("{{");
                result.push_str(&remaining[..end_idx + 2]);
            }
            current = &remaining[end_idx + 2..];
        } else {
            result.push_str("{{");
            current = remaining;
        }
    }
    result.push_str(current);
    result
}

/// Recursively scan and inject secrets into a serde_json::Value.
pub fn inject_value(value: &mut serde_json::Value, secrets: &HashMap<String, TaintedString>) {
    match value {
        serde_json::Value::String(s) => {
            *s = inject_str(s, secrets);
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                inject_value(v, secrets);
            }
        }
        serde_json::Value::Object(obj) => {
            for v in obj.values_mut() {
                inject_value(v, secrets);
            }
        }
        _ => {}
    }
}
