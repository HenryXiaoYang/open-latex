//! Compare two PDFs for typesetting equality: page count and MediaBoxes, decoded page content
//! streams byte for byte, embedded font programs and image XObjects by hash. Document-level
//! metadata (/ID, dates, producer) is ignored, as is object numbering.

use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Default)]
pub struct PdfDiff {
    pub equal: bool,
    pub pages_a: usize,
    pub pages_b: usize,
    pub differences: Vec<String>,
}

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn stream_bytes(doc: &Document, id: ObjectId) -> Option<Vec<u8>> {
    let obj = doc.get_object(id).ok()?;
    let s = obj.as_stream().ok()?;
    s.decompressed_content()
        .ok()
        .or_else(|| Some(s.content.clone()))
}

/// Resource fingerprints of a page: font programs and images, keyed by resource name.
fn page_resources(
    doc: &Document,
    page: ObjectId,
) -> Result<(BTreeMap<String, u64>, BTreeMap<String, u64>)> {
    let mut fonts = BTreeMap::new();
    let mut images = BTreeMap::new();
    let (res, res_ids) = doc.get_page_resources(page)?;
    let mut dicts: Vec<Dictionary> = Vec::new();
    if let Some(r) = res {
        dicts.push(r.clone());
    }
    for id in res_ids {
        if let Ok(d) = doc.get_dictionary(id) {
            dicts.push(d.clone());
        }
    }
    for d in dicts {
        if let Ok(fd) = d
            .get(b"Font")
            .and_then(|f| doc.dereference(f))
            .map(|(_, o)| o.clone())
        {
            if let Ok(fd) = fd.as_dict() {
                for (name, obj) in fd.iter() {
                    let name = String::from_utf8_lossy(name).to_string();
                    let mut h = 0u64;
                    if let Ok(fid) = obj.as_reference() {
                        if let Ok(fdict) = doc.get_dictionary(fid) {
                            // follow to FontDescriptor/FontFile*
                            let mut desc: Option<ObjectId> = fdict
                                .get(b"FontDescriptor")
                                .ok()
                                .and_then(|o| o.as_reference().ok());
                            if desc.is_none() {
                                if let Ok(df) = fdict.get(b"DescendantFonts") {
                                    if let Ok((_, Object::Array(arr))) = doc.dereference(df) {
                                        if let Some(Object::Reference(cid)) = arr.first() {
                                            if let Ok(cd) = doc.get_dictionary(*cid) {
                                                desc = cd
                                                    .get(b"FontDescriptor")
                                                    .ok()
                                                    .and_then(|o| o.as_reference().ok());
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(did) = desc {
                                if let Ok(dd) = doc.get_dictionary(did) {
                                    for key in [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"]
                                    {
                                        if let Ok(ff) = dd.get(key).and_then(|o| o.as_reference()) {
                                            if let Some(b) = stream_bytes(doc, ff) {
                                                h = hash(&b);
                                            }
                                        }
                                    }
                                }
                            }
                            let base = fdict
                                .get(b"BaseFont")
                                .ok()
                                .and_then(|o| o.as_name().ok())
                                .map(|n| String::from_utf8_lossy(n).to_string())
                                .unwrap_or_default();
                            // subset prefixes are random per run; strip them
                            let base = base.split('+').last().unwrap_or("").to_string();
                            h ^= hash(base.as_bytes());
                        }
                    }
                    fonts.insert(name, h);
                }
            }
        }
        if let Ok(xd) = d
            .get(b"XObject")
            .and_then(|f| doc.dereference(f))
            .map(|(_, o)| o.clone())
        {
            if let Ok(xd) = xd.as_dict() {
                for (name, obj) in xd.iter() {
                    let name = String::from_utf8_lossy(name).to_string();
                    if let Ok(xid) = obj.as_reference() {
                        if let Some(b) = stream_bytes(doc, xid) {
                            images.insert(name, hash(&b));
                        }
                    }
                }
            }
        }
    }
    Ok((fonts, images))
}

pub fn compare(a: &Path, b: &Path) -> Result<PdfDiff> {
    let da = Document::load(a).with_context(|| format!("loading {}", a.display()))?;
    let db = Document::load(b).with_context(|| format!("loading {}", b.display()))?;
    let pa = da.get_pages();
    let pb = db.get_pages();
    let mut diff = PdfDiff {
        equal: true,
        pages_a: pa.len(),
        pages_b: pb.len(),
        differences: vec![],
    };
    if pa.len() != pb.len() {
        diff.equal = false;
        diff.differences
            .push(format!("page count {} vs {}", pa.len(), pb.len()));
    }
    for (n, ida) in &pa {
        let Some(idb) = pb.get(n) else { continue };
        let ca = da.get_page_content(*ida)?;
        let cb = db.get_page_content(*idb)?;
        if ca != cb {
            diff.equal = false;
            let first = ca
                .iter()
                .zip(cb.iter())
                .position(|(x, y)| x != y)
                .unwrap_or(ca.len().min(cb.len()));
            let ctx_a =
                String::from_utf8_lossy(&ca[first.saturating_sub(40)..(first + 60).min(ca.len())])
                    .to_string();
            let ctx_b =
                String::from_utf8_lossy(&cb[first.saturating_sub(40)..(first + 60).min(cb.len())])
                    .to_string();
            diff.differences.push(format!(
                "page {n}: content differs at byte {first}: {:?} vs {:?}",
                ctx_a, ctx_b
            ));
        }
        let mb_a = da
            .get_dictionary(*ida)
            .ok()
            .and_then(|d| d.get(b"MediaBox").ok().cloned());
        let mb_b = db
            .get_dictionary(*idb)
            .ok()
            .and_then(|d| d.get(b"MediaBox").ok().cloned());
        if format!("{mb_a:?}") != format!("{mb_b:?}") {
            diff.equal = false;
            diff.differences
                .push(format!("page {n}: MediaBox {mb_a:?} vs {mb_b:?}"));
        }
        let (fa, ia) = page_resources(&da, *ida)?;
        let (fb, ib) = page_resources(&db, *idb)?;
        if fa != fb {
            diff.equal = false;
            diff.differences.push(format!(
                "page {n}: fonts differ ({} vs {})",
                fa.len(),
                fb.len()
            ));
        }
        if ia != ib {
            diff.equal = false;
            diff.differences.push(format!(
                "page {n}: images differ ({} vs {})",
                ia.len(),
                ib.len()
            ));
        }
        if diff.differences.len() > 20 {
            break;
        }
    }
    Ok(diff)
}
