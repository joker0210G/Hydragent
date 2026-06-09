use std::fmt;
use zeroize::Zeroize;

#[derive(Clone, Default, Zeroize)]
pub struct TaintedString(String);

impl TaintedString {
    pub fn new(s: String) -> Self {
        Self(s)
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl From<String> for TaintedString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for TaintedString {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl fmt::Debug for TaintedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl fmt::Display for TaintedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl serde::Serialize for TaintedString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for TaintedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self(s))
    }
}
