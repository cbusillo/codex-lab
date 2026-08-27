use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CanonicalJsonError {
    #[error("canonical JSON number at {location} must be a signed 64-bit integer")]
    InvalidNumber { location: String },
}

pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, CanonicalJsonError> {
    let mut output = String::new();
    write_value(value, "$", &mut output)?;
    Ok(output.into_bytes())
}

pub fn canonical_json_sha256(value: &Value) -> Result<String, CanonicalJsonError> {
    let bytes = canonical_json_bytes(value)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn write_value(
    value: &Value,
    location: &str,
    output: &mut String,
) -> Result<(), CanonicalJsonError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            let Some(value) = number.as_i64() else {
                return Err(CanonicalJsonError::InvalidNumber {
                    location: location.to_owned(),
                });
            };
            output.push_str(&value.to_string());
        }
        Value::String(value) => write_string(value, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_value(value, &format!("{location}[{index}]"), output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by_key(|(key, _)| key.as_str());
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_string(key, output);
                output.push(':');
                write_value(value, &format!("{location}.{key}"), output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn write_string(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{0c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            character if character <= '\u{1f}' || character == '\u{7f}' => {
                output.push_str(&format!("\\u{:04x}", character as u32));
            }
            character if character.is_ascii() => output.push(character),
            character if (character as u32) <= 0xffff => {
                output.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => {
                let code = character as u32 - 0x1_0000;
                let high = 0xd800 + (code >> 10);
                let low = 0xdc00 + (code & 0x3ff);
                output.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
            }
        }
    }
    output.push('"');
}
