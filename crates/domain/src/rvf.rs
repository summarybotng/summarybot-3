//! RVF (RuVector Format) binary export of knowledge units (ADR-117).
//!
//! A compact, self-describing binary for exporting knowledge units + their
//! embeddings for offline analysis, backup, and cross-system sharing. Pure: the
//! host/API gathers units and calls [`encode`]; this module owns only the byte
//! layout. All multi-byte integers are little-endian.
//!
//! Layout (ADR-117, with a self-describing header — the spec's fixed 64-byte
//! header can't hold a variable-length workspace id, so the id is
//! length-prefixed and the header ends after the checksum):
//!
//! ```text
//! Header: magic "RVF1" (4) | version u16 (1) | flags u16 | embedding_dim u32 |
//!         unit_count u32 | ws_id_len u16 | ws_id (utf-8) | created_at_ms i64 |
//!         checksum u32 (CRC32-IEEE of the records block)
//! Record: record_len u32 | id_len u16 | id | unit_type u8 | content_len u32 |
//!         content | source_id_len u16 | source_id | source_channel_len u16 |
//!         source_channel | source_date u32 (unix days) | confidence f32 |
//!         has_embedding u8 | [embedding f32 * embedding_dim]
//! ```

/// One knowledge unit to export (ADR-117 record). `embedding`, when present,
/// must have the same length as every other unit's (the file's `embedding_dim`).
#[derive(Debug, Clone, PartialEq)]
pub struct RvfUnit {
    pub id: String,
    /// ADR-117 unit-type enum (0 claim, 1 decision, 2 question, 3 action_item,
    /// 4 context, 5 definition, 6 reference).
    pub unit_type: u8,
    pub content: String,
    pub source_id: String,
    pub source_channel: String,
    /// Source date as whole days since the Unix epoch.
    pub source_date_days: u32,
    pub confidence: f32,
    pub embedding: Option<Vec<f32>>,
}

const MAGIC: &[u8; 4] = b"RVF1";
const VERSION: u16 = 1;

/// CRC32 (IEEE 802.3, reflected) over `data` — the records-block checksum.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Encode `units` to RVF bytes (ADR-117). `embedding_dim` is the vector length
/// (0 when embeddings are excluded); a unit whose embedding length doesn't match
/// is written as `has_embedding = 0`. `created_at_ms` is the export instant.
pub fn encode(
    units: &[RvfUnit],
    workspace_id: &str,
    embedding_dim: u32,
    created_at_ms: i64,
) -> Vec<u8> {
    // Records block first, so the header can carry its checksum.
    let mut records = Vec::new();
    for u in units {
        let mut rec = Vec::new();
        put_str(&mut rec, &u.id);
        rec.push(u.unit_type);
        rec.extend_from_slice(&(u.content.len() as u32).to_le_bytes());
        rec.extend_from_slice(u.content.as_bytes());
        put_str(&mut rec, &u.source_id);
        put_str(&mut rec, &u.source_channel);
        rec.extend_from_slice(&u.source_date_days.to_le_bytes());
        rec.extend_from_slice(&u.confidence.to_le_bytes());
        let emb = u
            .embedding
            .as_ref()
            .filter(|e| embedding_dim > 0 && e.len() as u32 == embedding_dim);
        match emb {
            Some(e) => {
                rec.push(1);
                for f in e {
                    rec.extend_from_slice(&f.to_le_bytes());
                }
            }
            None => rec.push(0),
        }
        // Length-prefix the whole record so a reader can skip it.
        records.extend_from_slice(&(rec.len() as u32).to_le_bytes());
        records.extend_from_slice(&rec);
    }

    let mut out = Vec::with_capacity(64 + records.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags (reserved)
    out.extend_from_slice(&embedding_dim.to_le_bytes());
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    put_str(&mut out, workspace_id);
    out.extend_from_slice(&created_at_ms.to_le_bytes());
    out.extend_from_slice(&crc32(&records).to_le_bytes());
    out.extend_from_slice(&records);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: &str, emb: Option<Vec<f32>>) -> RvfUnit {
        RvfUnit {
            id: id.into(),
            unit_type: 0,
            content: "we shipped".into(),
            source_id: "m0".into(),
            source_channel: "general".into(),
            source_date_days: 20_000,
            confidence: 0.9,
            embedding: emb,
        }
    }

    #[test]
    fn header_carries_magic_version_count_and_checksum() {
        let units = vec![unit("u0", None), unit("u1", None)];
        let bytes = encode(&units, "ws-1", 0, 1_700_000_000_000);
        assert_eq!(&bytes[0..4], b"RVF1");
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), VERSION);
        // embedding_dim (offset 8) = 0; unit_count (offset 12) = 2.
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 2);
        // ws id length + value follow.
        assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 4);
        assert_eq!(&bytes[18..22], b"ws-1");
    }

    #[test]
    fn checksum_matches_records_block() {
        let units = vec![unit("u0", None)];
        let bytes = encode(&units, "w", 0, 0);
        // Header = 4+2+2+4+4+ (2+1) +8+4 = 31 bytes for ws id "w".
        let header_len = 4 + 2 + 2 + 4 + 4 + 2 + 1 + 8 + 4;
        let stored = u32::from_le_bytes(
            bytes[header_len - 4..header_len].try_into().unwrap(),
        );
        assert_eq!(stored, crc32(&bytes[header_len..]));
    }

    #[test]
    fn embeddings_are_written_only_when_dim_matches() {
        // dim 3 → the matching embedding is written (has_embedding=1).
        let with = encode(&[unit("u", Some(vec![0.1, 0.2, 0.3]))], "w", 3, 0);
        // dim 0 → embeddings excluded even though the unit has one.
        let without = encode(&[unit("u", Some(vec![0.1, 0.2, 0.3]))], "w", 0, 0);
        assert!(with.len() > without.len());
        // A wrong-length embedding under dim 3 is dropped, not corrupting the file.
        let mismatched = encode(&[unit("u", Some(vec![0.1, 0.2]))], "w", 3, 0);
        assert_eq!(mismatched.len(), without.len());
    }
}
