use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::CanonicalJsonError;
use crate::canonical_json_sha256;
use crate::decode::deserialize_value;

pub const OWNER_CONTROL_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ErrorLocation {
    Field(String),
    Index(usize),
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("{field} must be {rule}")]
    Field {
        field: &'static str,
        rule: &'static str,
    },
    #[error("{rule}")]
    Rule { rule: &'static str },
    #[error("{source}")]
    Nested {
        prefix: Vec<ErrorLocation>,
        source: Box<ValidationError>,
    },
    #[error("{source}")]
    Json {
        location: Vec<ErrorLocation>,
        source: serde_json::Error,
    },
    #[error(transparent)]
    CanonicalJson(#[from] CanonicalJsonError),
}

impl ValidationError {
    pub fn location(&self) -> Vec<ErrorLocation> {
        match self {
            Self::Field { field, .. } => vec![ErrorLocation::Field((*field).to_owned())],
            Self::Nested { prefix, source } => {
                let mut location = prefix.clone();
                location.extend(source.location());
                location
            }
            Self::Json { location, .. } => location.clone(),
            Self::Rule { .. } | Self::CanonicalJson(_) => Vec::new(),
        }
    }

    pub(crate) fn at_field(self, field: &'static str) -> Self {
        Self::Nested {
            prefix: vec![ErrorLocation::Field(field.to_owned())],
            source: Box::new(self),
        }
    }

    fn at_index(self, index: usize) -> Self {
        Self::Nested {
            prefix: vec![ErrorLocation::Index(index)],
            source: Box::new(self),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum DescriptorId {
    ManagedSecretReencryption,
    ManagedAuthzPolicySet,
    ManagedMergeTrainPolicyImport,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum Decision {
    #[serde(rename = "approved")]
    Approved,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewItem {
    pub key: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServerReviewPayload {
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
    pub review_id: String,
    pub title: String,
    pub summary: String,
    pub items: Vec<ReviewItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
    pub operation_id: String,
    pub descriptor_id: DescriptorId,
    pub descriptor_version: u8,
    pub request_digest: String,
    pub plan_digest: String,
    pub evidence_digest: String,
    pub pre_state_digest: String,
    pub policy_record_id: String,
    pub policy_revision: i64,
    pub policy_sha256: String,
    pub owner_github_id: i64,
    pub server_review: ServerReviewPayload,
    pub nonce: String,
    pub issued_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChallengeResponse {
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
    pub approval_request: ApprovalRequest,
    pub approval_request_digest: String,
    pub decision: Decision,
    pub channel_binding_sha256: String,
    pub confirmed_at: String,
}

impl ReviewItem {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_pattern(
            self.key.as_bytes(),
            "key",
            is_review_key,
            64,
            "a canonical review key",
        )?;
        validate_text_length(&self.label, "label", 1, 120)?;
        validate_text_length(&self.value, "value", 1, 4000)?;
        if has_python_surrounding_whitespace(&self.label)
            || has_python_surrounding_whitespace(&self.value)
        {
            return Err(ValidationError::Rule {
                rule: "review item labels and values must not have surrounding whitespace",
            });
        }
        Ok(())
    }
}

impl ServerReviewPayload {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let payload: Self = deserialize_value(value)?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        validate_identifier(&self.review_id, "review_id")?;
        validate_text_length(&self.title, "title", 1, 200)?;
        validate_text_length(&self.summary, "summary", 1, 4000)?;
        if !(1..=32).contains(&self.items.len()) {
            return Err(ValidationError::Field {
                field: "items",
                rule: "contain between 1 and 32 items",
            });
        }
        for (index, item) in self.items.iter().enumerate() {
            item.validate()
                .map_err(|error| error.at_index(index).at_field("items"))?;
        }
        if has_python_surrounding_whitespace(&self.title)
            || has_python_surrounding_whitespace(&self.summary)
        {
            return Err(ValidationError::Rule {
                rule: "review title and summary must not have surrounding whitespace",
            });
        }
        let mut keys = std::collections::HashSet::new();
        if self.items.iter().any(|item| !keys.insert(&item.key)) {
            return Err(ValidationError::Rule {
                rule: "review item keys must be unique",
            });
        }
        Ok(())
    }
}

impl ApprovalRequest {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let payload: Self = deserialize_value(value)?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        validate_operation_id(&self.operation_id)?;
        if self.descriptor_version != 1 {
            return Err(ValidationError::Field {
                field: "descriptor_version",
                rule: "exactly 1",
            });
        }
        for (field, value) in [
            ("request_digest", &self.request_digest),
            ("plan_digest", &self.plan_digest),
            ("evidence_digest", &self.evidence_digest),
            ("pre_state_digest", &self.pre_state_digest),
        ] {
            validate_sha256(value, field)?;
        }
        validate_identifier(&self.policy_record_id, "policy_record_id")?;
        if !(1..=i64::MAX).contains(&self.policy_revision) {
            return Err(ValidationError::Field {
                field: "policy_revision",
                rule: "a positive signed 64-bit integer",
            });
        }
        validate_sha256(&self.policy_sha256, "policy_sha256")?;
        if !(1..=i64::MAX).contains(&self.owner_github_id) {
            return Err(ValidationError::Field {
                field: "owner_github_id",
                rule: "a positive signed 64-bit integer",
            });
        }
        self.server_review
            .validate()
            .map_err(|error| error.at_field("server_review"))?;
        validate_nonce(&self.nonce)?;
        let issued_at = parse_timestamp(&self.issued_at, "issued_at")?;
        let expires_at = parse_timestamp(&self.expires_at, "expires_at")?;
        if expires_at <= issued_at {
            return Err(ValidationError::Rule {
                rule: "expires_at must be later than issued_at",
            });
        }
        Ok(())
    }
}

impl ChallengeResponse {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let payload: Self = deserialize_value(value)?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        self.approval_request
            .validate()
            .map_err(|error| error.at_field("approval_request"))?;
        validate_sha256(&self.approval_request_digest, "approval_request_digest")?;
        validate_sha256(&self.channel_binding_sha256, "channel_binding_sha256")?;
        let confirmed_at = parse_timestamp(&self.confirmed_at, "confirmed_at")?;
        let issued_at = parse_timestamp(&self.approval_request.issued_at, "issued_at")?;
        let expires_at = parse_timestamp(&self.approval_request.expires_at, "expires_at")?;
        if self.approval_request_digest != approval_request_digest(&self.approval_request)? {
            return Err(ValidationError::Rule {
                rule: "approval_request_digest must bind the exact approval request",
            });
        }
        if confirmed_at < issued_at {
            return Err(ValidationError::Rule {
                rule: "confirmed_at must not be earlier than issued_at",
            });
        }
        if confirmed_at > expires_at {
            return Err(ValidationError::Rule {
                rule: "confirmed_at must not be later than expires_at",
            });
        }
        Ok(())
    }
}

pub fn approval_request_digest(request: &ApprovalRequest) -> Result<String, ValidationError> {
    let value = serde_json::to_value(request).map_err(|source| ValidationError::Json {
        location: Vec::new(),
        source,
    })?;
    Ok(canonical_json_sha256(&value)?)
}

pub fn challenge_response_digest(response: &ChallengeResponse) -> Result<String, ValidationError> {
    let value = serde_json::to_value(response).map_err(|source| ValidationError::Json {
        location: Vec::new(),
        source,
    })?;
    Ok(canonical_json_sha256(&value)?)
}

fn default_schema_version() -> u8 {
    OWNER_CONTROL_SCHEMA_VERSION
}

fn validate_text_length(
    value: &str,
    field: &'static str,
    minimum: usize,
    maximum: usize,
) -> Result<(), ValidationError> {
    if !(minimum..=maximum).contains(&value.chars().count()) {
        return Err(ValidationError::Field {
            field,
            rule: "non-empty and length-bounded",
        });
    }
    Ok(())
}

pub(crate) fn validate_identifier(value: &str, field: &'static str) -> Result<(), ValidationError> {
    validate_pattern(
        value.as_bytes(),
        field,
        is_identifier,
        128,
        "a canonical identifier",
    )
}

fn validate_sha256(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ValidationError::Field {
            field,
            rule: "a lowercase SHA-256 digest",
        });
    }
    Ok(())
}

fn validate_nonce(value: &str) -> Result<(), ValidationError> {
    if !(16..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ValidationError::Field {
            field: "nonce",
            rule: "a 16-to-128 character canonical nonce",
        });
    }
    Ok(())
}

fn validate_operation_id(value: &str) -> Result<(), ValidationError> {
    let prefix = "privileged-operation-";
    if !value.starts_with(prefix)
        || value[prefix.len()..].len() != 32
        || !value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ValidationError::Field {
            field: "operation_id",
            rule: "a value matching privileged-operation-[0-9a-f]{32}",
        });
    }
    Ok(())
}

fn validate_pattern(
    value: &[u8],
    field: &'static str,
    predicate: fn(u8, usize) -> bool,
    maximum: usize,
    rule: &'static str,
) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > maximum
        || !value
            .iter()
            .enumerate()
            .all(|(index, byte)| predicate(*byte, index))
    {
        return Err(ValidationError::Field { field, rule });
    }
    Ok(())
}

fn is_identifier(byte: u8, index: usize) -> bool {
    byte.is_ascii_lowercase() && index == 0
        || index > 0
            && (byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-'))
}

fn is_review_key(byte: u8, index: usize) -> bool {
    byte.is_ascii_lowercase() && index == 0
        || index > 0 && (byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn has_python_surrounding_whitespace(value: &str) -> bool {
    value.chars().next().is_some_and(is_python_whitespace)
        || value.chars().next_back().is_some_and(is_python_whitespace)
}

fn is_python_whitespace(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timestamp {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

pub(crate) fn parse_timestamp(
    value: &str,
    field: &'static str,
) -> Result<Timestamp, ValidationError> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 25
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && &bytes[19..] == b"+00:00";
    if !valid_shape
        || !bytes[..19]
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
    {
        return Err(ValidationError::Field {
            field,
            rule: "in canonical UTC form YYYY-MM-DDTHH:MM:SS+00:00",
        });
    }
    let number = |start, end| value[start..end].parse::<u16>().unwrap_or(0);
    let timestamp = Timestamp {
        year: number(0, 4),
        month: number(5, 7) as u8,
        day: number(8, 10) as u8,
        hour: number(11, 13) as u8,
        minute: number(14, 16) as u8,
        second: number(17, 19) as u8,
    };
    let days_in_month = match timestamp.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(timestamp.year) => 29,
        2 => 28,
        _ => 0,
    };
    if timestamp.year == 0
        || timestamp.day == 0
        || timestamp.day > days_in_month
        || timestamp.hour > 23
        || timestamp.minute > 59
        || timestamp.second > 59
    {
        return Err(ValidationError::Rule {
            rule: "timestamps must be calendar-valid whole-second UTC values",
        });
    }
    Ok(timestamp)
}

fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}
