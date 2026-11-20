use std::{error::Error, fmt};

/// User-visible mapping context kept separate from the underlying transport error.
#[derive(Debug)]
pub struct MappingMessage {
    pub key: &'static str,
    pub value: Option<String>,
    detail: Option<anyhow::Error>,
}

impl MappingMessage {
    pub fn new(key: &'static str) -> Self {
        Self { key, value: None, detail: None }
    }

    pub(super) fn with_value(mut self, value: impl ToString) -> Self {
        self.value = Some(value.to_string());
        self
    }

    pub(super) fn caused_by(mut self, error: impl Into<anyhow::Error>) -> Self {
        self.detail = Some(error.into());
        self
    }
}

impl fmt::Display for MappingMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.value.as_deref() {
            Some(value) => f.write_str(&self.key.replacen("{}", value, 1)),
            None => f.write_str(self.key),
        }
    }
}

impl Error for MappingMessage {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.detail.as_ref().map(|error| error.as_ref() as &(dyn Error + 'static))
    }
}
