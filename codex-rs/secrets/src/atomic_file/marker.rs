use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use sha2::Digest;
use sha2::Sha256;

use super::TransactionKind;

const MAGIC: [u8; 8] = *b"ECATOMIC";
const VERSION: u8 = 1;
const LEN: usize = 74;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkerRecord {
    pub(super) source: [u8; 32],
    pub(super) replacement: [u8; 32],
}

impl MarkerRecord {
    pub(super) fn new(
        kind: TransactionKind,
        source: Option<&[u8]>,
        replacement: &[u8],
    ) -> Result<Self> {
        let source = match kind {
            TransactionKind::FirstPublish => {
                anyhow::ensure!(source.is_none(), "first-publish transaction has a source");
                [0; 32]
            }
            TransactionKind::ReplaceExisting => fingerprint(
                source
                    .with_context(|| "replacement transaction is missing its source generation")?,
            ),
        };
        Ok(Self {
            source,
            replacement: fingerprint(replacement),
        })
    }

    pub(super) fn encode(self, kind: TransactionKind) -> [u8; LEN] {
        let mut bytes = [0; LEN];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8] = VERSION;
        bytes[9] = kind.marker_byte();
        bytes[10..42].copy_from_slice(&self.source);
        bytes[42..].copy_from_slice(&self.replacement);
        bytes
    }

    pub(super) fn decode(bytes: &[u8], kind: TransactionKind, path: &Path) -> Result<Self> {
        anyhow::ensure!(
            bytes.len() == LEN,
            "invalid secrets transaction marker {}; preserving it",
            path.display()
        );
        anyhow::ensure!(
            bytes.starts_with(&MAGIC) && bytes[8] == VERSION,
            "unsupported secrets transaction marker {}; preserving it",
            path.display()
        );
        anyhow::ensure!(
            TransactionKind::from_marker_byte(bytes[9]) == Some(kind),
            "secrets transaction marker kind mismatch at {}; preserving it",
            path.display()
        );
        let mut source = [0; 32];
        source.copy_from_slice(&bytes[10..42]);
        anyhow::ensure!(
            kind == TransactionKind::ReplaceExisting || source == [0; 32],
            "first-publish marker {} contains a source generation; preserving it",
            path.display()
        );
        let mut replacement = [0; 32];
        replacement.copy_from_slice(&bytes[42..]);
        Ok(Self {
            source,
            replacement,
        })
    }
}

pub(super) fn fingerprint_file(path: &Path) -> Result<[u8; 32]> {
    let contents = fs::read(path)
        .with_context(|| format!("failed to validate transaction file {}", path.display()))?;
    Ok(fingerprint(&contents))
}

fn fingerprint(contents: &[u8]) -> [u8; 32] {
    Sha256::digest(contents).into()
}
