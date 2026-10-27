use crate::{engine, extract::Documents, openapi};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub service: String,
    pub pointer: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
    pub affected_operations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub service: String,
    pub classification: String,
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Comparison {
    pub changes: Vec<Change>,
    pub findings: Vec<Finding>,
    pub compatibility_engine: Option<String>,
    pub comparison_ms: f64,
}
impl Comparison {
    pub fn breaking(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.classification == "breaking")
            .count()
    }
}

fn documentation(path: &[String]) -> bool {
    let Some(last) = path.last() else {
        return false;
    };
    if path.len() >= 2
        && ["properties", "schemas", "$defs", "definitions"]
            .contains(&path[path.len() - 2].as_str())
    {
        return false;
    }
    [
        "description",
        "summary",
        "example",
        "examples",
        "externalDocs",
        "tags",
        "operationId",
    ]
    .contains(&last.as_str())
        || path.first().is_some_and(|s| s == "info")
}

fn raw_diff(
    service: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    path: &mut Vec<String>,
    changes: &mut Vec<Change>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Some(Value::Object(old)), Some(Value::Object(new))) => {
            let keys: BTreeSet<_> = old.keys().chain(new.keys()).collect();
            for key in keys {
                path.push(key.clone());
                raw_diff(service, old.get(key), new.get(key), path, changes);
                path.pop();
            }
        }
        _ => changes.push(Change {
            service: service.into(),
            pointer: openapi::pointer(path),
            kind: if documentation(path) {
                "documentation"
            } else {
                "structural"
            }
            .into(),
            before: before.cloned(),
            after: after.cloned(),
            affected_operations: Vec::new(),
        }),
    }
}

fn related(pointer: &str, target: &str) -> bool {
    pointer == target
        || pointer
            .strip_prefix(target)
            .is_some_and(|s| s.starts_with('/'))
        || target
            .strip_prefix(pointer)
            .is_some_and(|s| s.starts_with('/'))
}

pub fn structural(before: &Documents, after: &Documents) -> Result<Comparison> {
    let mut comparison = Comparison::default();
    let services: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    for service in services {
        let start_index = comparison.changes.len();
        raw_diff(
            service,
            before.get(service),
            after.get(service),
            &mut Vec::new(),
            &mut comparison.changes,
        );
        let mut operations = BTreeMap::<String, Vec<openapi::Operation>>::new();
        for document in [before.get(service), after.get(service)]
            .into_iter()
            .flatten()
        {
            for (identity, operation) in openapi::operations(document)? {
                operations.entry(identity).or_default().push(operation);
            }
        }
        for change in &mut comparison.changes[start_index..] {
            for (identity, variants) in &operations {
                let affected = variants.iter().any(|operation| {
                    related(&change.pointer, &operation.source_pointer)
                        || operation
                            .source_pointer
                            .rsplit_once('/')
                            .is_some_and(|(parent, _)| {
                                if !related(&change.pointer, parent) {
                                    return false;
                                }
                                let tail = change
                                    .pointer
                                    .strip_prefix(parent)
                                    .unwrap_or("")
                                    .trim_start_matches('/');
                                let first = tail.split('/').next().unwrap_or("");
                                !openapi::METHODS.contains(&first)
                            })
                        || operation.referenced_definitions.keys().any(|reference| {
                            related(&change.pointer, reference.trim_start_matches('#'))
                        })
                }) || (["/security", "/servers"]
                    .iter()
                    .any(|p| related(&change.pointer, p))
                    && (variants.len() < 2
                        || variants[0].security != variants[1].security
                        || variants[0].servers != variants[1].servers));
                if affected {
                    change.affected_operations.push(identity.clone());
                }
            }
        }
    }
    Ok(comparison)
}

pub fn compare(before: &Documents, after: &Documents, timeout: Duration) -> Result<Comparison> {
    let start = Instant::now();
    let mut comparison = structural(before, after)?;
    // Requiring a known engine even for unchanged inputs prevents CI from silently
    // passing with compatibility analysis unavailable.
    let engine_path = engine::locate()?;
    comparison.compatibility_engine = Some(format!("oasdiff {}", engine::VERSION));
    let services: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    for service in services {
        if before.get(service) == after.get(service) {
            continue;
        }
        let old_empty;
        let new_empty;
        let old = match before.get(service) {
            Some(doc) => doc,
            None => {
                old_empty = openapi::empty_like(&after[service]);
                &old_empty
            }
        };
        let new = match after.get(service) {
            Some(doc) => doc,
            None => {
                new_empty = openapi::empty_like(&before[service]);
                &new_empty
            }
        };
        for finding in engine::changelog(&engine_path, old, new, timeout)? {
            let classification = match finding.get("level").and_then(Value::as_u64) {
                Some(3..) => "breaking",
                Some(2) => "warning",
                _ => "compatible",
            };
            comparison.findings.push(Finding {
                service: service.clone(),
                classification: classification.into(),
                detail: finding,
            });
        }
    }
    comparison.comparison_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(comparison)
}
