use crate::util;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

pub const METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

pub fn pointer(path: &[String]) -> String {
    path.iter()
        .map(|part| format!("/{}", util::escape_pointer(part)))
        .collect()
}

fn literal(path: &[String], key: &str, value: &Value) -> bool {
    // Property/component names can be any string, including "example" and "default".
    if path.last().is_some_and(|p| {
        [
            "properties",
            "patternProperties",
            "$defs",
            "schemas",
            "definitions",
        ]
        .contains(&p.as_str())
    }) {
        return false;
    }
    ["example", "default", "const", "enum"].contains(&key)
        || (key == "examples" && value.is_array())
        || (key == "value" && path.len() >= 2 && path[path.len() - 2] == "examples")
}

fn walk_refs(value: &Value, path: &mut Vec<String>, output: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                output.push((format!("{}/$ref", pointer(path)), reference.to_owned()));
            }
            for (key, child) in map {
                if !literal(path, key, child) {
                    path.push(key.clone());
                    walk_refs(child, path, output);
                    path.pop();
                }
            }
        }
        Value::Array(array) => {
            for (i, child) in array.iter().enumerate() {
                path.push(i.to_string());
                walk_refs(child, path, output);
                path.pop();
            }
        }
        _ => {}
    }
}

pub fn references(value: &Value) -> Vec<(String, String)> {
    let mut result = Vec::new();
    walk_refs(value, &mut Vec::new(), &mut result);
    result
}

fn anchor_pointer(value: &Value, anchor: &str, path: &mut Vec<String>, matches: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if map.get("$anchor").and_then(Value::as_str) == Some(anchor) {
                matches.push(pointer(path));
            }
            for (key, child) in map {
                if !literal(path, key, child) {
                    path.push(key.clone());
                    anchor_pointer(child, anchor, path, matches);
                    path.pop();
                }
            }
        }
        Value::Array(array) => {
            for (i, child) in array.iter().enumerate() {
                path.push(i.to_string());
                anchor_pointer(child, anchor, path, matches);
                path.pop();
            }
        }
        _ => {}
    }
}

fn fragment_pointer(document: &Value, fragment: &str) -> Result<String> {
    let decoded = percent_encoding::percent_decode_str(fragment).decode_utf8()?;
    if decoded.is_empty() || decoded.starts_with('/') {
        return Ok(decoded.into_owned());
    }
    let mut matches = Vec::new();
    anchor_pointer(document, &decoded, &mut Vec::new(), &mut matches);
    ensure!(
        matches.len() == 1,
        "unresolved or ambiguous JSON Schema anchor #{decoded}"
    );
    Ok(matches.remove(0))
}

/// Bundle local documents without dereferencing: recursive schemas remain finite.
/// Every archived document is therefore usable independently of the source tree.
pub fn bundle(root: &Path, source: &Path, document: Value) -> Result<Value> {
    let root = root.canonicalize()?;
    let source = if source.exists() {
        source.canonicalize()?
    } else {
        source.to_owned()
    };
    let mut documents = BTreeMap::from([(source.clone(), document)]);
    let mut transformed = BTreeMap::new();
    let mut queue = VecDeque::from([source.clone()]);
    while let Some(path) = queue.pop_front() {
        if transformed.contains_key(&path) {
            continue;
        }
        let mut doc = documents[&path].clone();
        for (location, reference) in references(&doc) {
            let (file, fragment) = reference.split_once('#').unwrap_or((&reference, ""));
            ensure!(
                !file.contains("://") && !file.starts_with("file:"),
                "remote references are unsupported in v1: {reference}; bundle them locally first"
            );
            let target_path = if file.is_empty() {
                path.clone()
            } else {
                let decoded = percent_encoding::percent_decode_str(file).decode_utf8()?;
                let candidate = path
                    .parent()
                    .context("reference has no base directory")?
                    .join(decoded.as_ref());
                let resolved = candidate.canonicalize().with_context(|| {
                    format!("resolve reference {reference} in {}", path.display())
                })?;
                ensure!(
                    resolved.starts_with(&root),
                    "reference escapes project root: {reference}"
                );
                resolved
            };
            if !documents.contains_key(&target_path) {
                documents.insert(target_path.clone(), util::read_json(&target_path)?);
                queue.push_back(target_path.clone());
            }
            let target = &documents[&target_path];
            let fragment = fragment_pointer(target, fragment)?;
            ensure!(
                target.pointer(&fragment).is_some(),
                "unresolved reference {reference} in {}",
                path.display()
            );
            let prefix = if target_path == source {
                String::new()
            } else {
                let relative = target_path.strip_prefix(&root)?;
                format!(
                    "/x-contracts-bundled/{}",
                    &util::hash(relative.to_string_lossy().as_bytes())[..24]
                )
            };
            *doc.pointer_mut(&location)
                .context("reference location disappeared")? =
                Value::String(format!("#{prefix}{fragment}"));
        }
        transformed.insert(path, doc);
    }
    let mut result = transformed
        .remove(&source)
        .context("missing root document")?;
    if !transformed.is_empty() {
        let bundled = result
            .as_object_mut()
            .context("OpenAPI must be an object")?
            .entry("x-contracts-bundled")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("x-contracts-bundled must be an object")?;
        for (path, value) in transformed {
            let key =
                util::hash(path.strip_prefix(&root)?.to_string_lossy().as_bytes())[..24].to_owned();
            ensure!(
                !bundled.contains_key(&key),
                "reserved x-contracts-bundled key collision"
            );
            bundled.insert(key, value);
        }
    }
    validate(&result)?;
    Ok(result)
}

pub fn validate(document: &Value) -> Result<()> {
    let version = document
        .get("openapi")
        .and_then(Value::as_str)
        .context("missing OpenAPI version")?;
    let parts: Vec<_> = version.split('.').collect();
    ensure!(
        parts.len() == 3
            && parts[0] == "3"
            && ["0", "1"].contains(&parts[1])
            && parts[2].parse::<u32>().is_ok(),
        "unsupported OpenAPI version {version}; supported: 3.0.x and 3.1.x"
    );
    ensure!(
        document
            .pointer("/info/title")
            .and_then(Value::as_str)
            .is_some(),
        "info.title is required"
    );
    ensure!(
        document
            .pointer("/info/version")
            .and_then(Value::as_str)
            .is_some(),
        "info.version is required"
    );
    let paths = document
        .get("paths")
        .and_then(Value::as_object)
        .context("paths must be an object")?;
    for (path, item) in paths {
        if path.starts_with("x-") {
            continue;
        }
        ensure!(
            path.starts_with('/'),
            "API path must start with '/': {path}"
        );
        let item = resolve(document, item)?;
        let map = item
            .as_object()
            .with_context(|| format!("path item {path} must be an object"))?;
        for method in METHODS {
            if let Some(operation) = map.get(*method) {
                ensure!(operation.is_object(), "{method} {path} must be an object");
                let responses = operation
                    .get("responses")
                    .and_then(Value::as_object)
                    .with_context(|| format!("{method} {path}: responses must be an object"))?;
                ensure!(
                    !responses.is_empty(),
                    "{method} {path}: responses cannot be empty"
                );
                for (status, response) in responses {
                    if status.starts_with("x-") {
                        continue;
                    }
                    let valid_status = status == "default"
                        || (status.len() == 3
                            && matches!(status.as_bytes()[0], b'1'..=b'5')
                            && (status.as_bytes()[1..].iter().all(u8::is_ascii_digit)
                                || &status[1..] == "XX"));
                    ensure!(valid_status, "invalid response status {status}");
                    ensure!(
                        resolve(document, response)?
                            .get("description")
                            .and_then(Value::as_str)
                            .is_some(),
                        "{method} {path}: response {status} needs a description"
                    );
                }
            }
        }
    }
    for (_, reference) in references(document) {
        let fragment = reference
            .strip_prefix('#')
            .with_context(|| format!("unbundled reference {reference}"))?;
        ensure!(
            document.pointer(fragment).is_some(),
            "unresolved reference {reference}"
        );
    }
    Ok(())
}

pub fn resolve<'a>(document: &'a Value, value: &'a Value) -> Result<&'a Value> {
    let mut current = value;
    let mut seen = BTreeSet::new();
    while let Some(reference) = current.get("$ref").and_then(Value::as_str) {
        ensure!(
            seen.insert(reference),
            "cyclic non-schema reference {reference}"
        );
        current = document
            .pointer(reference.strip_prefix('#').context("unbundled reference")?)
            .with_context(|| format!("unresolved reference {reference}"))?;
    }
    Ok(current)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub identity: String,
    pub method: String,
    pub path: String,
    pub tag: String,
    pub source_pointer: String,
    pub operation: Value,
    pub parameters: Vec<Value>,
    pub security: Value,
    pub servers: Value,
    pub referenced_definitions: BTreeMap<String, Value>,
}

pub fn operations(document: &Value) -> Result<BTreeMap<String, Operation>> {
    let mut output = BTreeMap::new();
    for (path, path_item) in document["paths"].as_object().context("missing paths")? {
        if path.starts_with("x-") {
            continue;
        }
        let item = resolve(document, path_item)?;
        let mut source_base = format!("/paths/{}", util::escape_pointer(path));
        let mut source_item = path_item;
        while let Some(reference) = source_item.get("$ref").and_then(Value::as_str) {
            source_base = reference
                .strip_prefix('#')
                .context("unbundled path reference")?
                .to_owned();
            source_item = document
                .pointer(&source_base)
                .context("missing path reference")?;
        }
        for method in METHODS {
            let Some(operation) = item.get(*method) else {
                continue;
            };
            let identity = format!("{} {path}", method.to_uppercase());
            let tag = operation
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|tags| tags.first())
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    path.split('/')
                        .find(|s| !s.is_empty() && !s.starts_with('{'))
                        .unwrap_or("default")
                        .to_owned()
                });
            let mut parameters = BTreeMap::new();
            for container in [item.get("parameters"), operation.get("parameters")]
                .into_iter()
                .flatten()
            {
                for param in container
                    .as_array()
                    .context("parameters must be an array")?
                {
                    let resolved = resolve(document, param)?;
                    let name = resolved
                        .get("name")
                        .and_then(Value::as_str)
                        .context("parameter needs a name")?;
                    let location = resolved
                        .get("in")
                        .and_then(Value::as_str)
                        .context("parameter needs an 'in' location")?;
                    ensure!(
                        ["path", "query", "header", "cookie"].contains(&location),
                        "invalid parameter location {location}"
                    );
                    ensure!(
                        location != "path"
                            || resolved.get("required").and_then(Value::as_bool) == Some(true),
                        "path parameter {name} must be required"
                    );
                    parameters.insert((location.to_owned(), name.to_owned()), resolved.clone());
                }
            }
            let parameters: Vec<_> = parameters.into_values().collect();
            let security = operation
                .get("security")
                .or_else(|| document.get("security"))
                .cloned()
                .unwrap_or_else(|| json!([]));
            let servers = operation
                .get("servers")
                .or_else(|| item.get("servers"))
                .or_else(|| document.get("servers"))
                .cloned()
                .unwrap_or_else(|| json!([]));
            let mut definitions = BTreeMap::new();
            let mut pending = references(
                &json!({"operation":operation,"parameters":parameters,"security":security,"servers":servers}),
            );
            if let Some(parameters) = item.get("parameters") {
                pending.extend(references(parameters));
            }
            // Security schemes are referenced by name rather than $ref.
            if let Some(alternatives) = security.as_array() {
                for alternative in alternatives {
                    if let Some(requirement) = alternative.as_object() {
                        for name in requirement.keys() {
                            let p = format!(
                                "/components/securitySchemes/{}",
                                util::escape_pointer(name)
                            );
                            ensure!(
                                document.pointer(&p).is_some(),
                                "missing security scheme {name}"
                            );
                            pending.push((String::new(), format!("#{p}")));
                        }
                    }
                }
            }
            while let Some((_, reference)) = pending.pop() {
                if definitions.contains_key(&reference) {
                    continue;
                }
                let target = document
                    .pointer(reference.strip_prefix('#').context("unbundled reference")?)
                    .with_context(|| format!("unresolved reference {reference}"))?;
                definitions.insert(reference.clone(), target.clone());
                pending.extend(references(target));
            }
            output.insert(
                identity.clone(),
                Operation {
                    identity,
                    method: method.to_uppercase(),
                    path: path.clone(),
                    tag,
                    source_pointer: format!("{source_base}/{method}"),
                    operation: operation.clone(),
                    parameters,
                    security,
                    servers,
                    referenced_definitions: definitions,
                },
            );
        }
    }
    Ok(output)
}

/// Relative filenames and bytes for all generated, non-authoritative navigation views.
pub fn views(service: &str, document: &Value) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut index = Map::new();
    for (identity, operation) in operations(document)? {
        let digest = util::hash(format!("{service}\n{identity}").as_bytes());
        let name = operation
            .operation
            .get("operationId")
            .and_then(Value::as_str)
            .unwrap_or(&identity);
        // Hash the tag too: "Users" and "users" must not collide on case-insensitive filesystems.
        let tag_hash = util::hash(operation.tag.as_bytes());
        let group = format!("{}--{}", util::slug(&operation.tag), &tag_hash[..8]);
        let path = PathBuf::from(format!(
            "operations/{group}/{}--{}.json",
            util::slug(name),
            &digest[..16]
        ));
        files.insert(
            path.clone(),
            util::json_bytes(
                &json!({"service":service,"source":"openapi.json","contract":operation}),
            )?,
        );
        index.insert(identity, json!(path.to_string_lossy()));
    }
    let mut schema_index = Map::new();
    if let Some(schemas) = document
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    {
        let mut used = BTreeSet::new();
        let mut candidates = BTreeMap::new();
        let mut counts = BTreeMap::<String, usize>::new();
        for name in schemas.keys() {
            let safe = !name.is_empty()
                && name != "."
                && name != ".."
                && name.len() < 100
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || [b'.', b'-', b'_'].contains(&b));
            let base = if safe { name.clone() } else { util::slug(name) };
            let filename = format!("{base}.json");
            *counts.entry(filename.to_lowercase()).or_default() += 1;
            candidates.insert(name, filename);
        }
        let reserved: BTreeSet<_> = candidates
            .values()
            .map(|name| name.to_lowercase())
            .collect();
        for (name, schema) in schemas {
            let candidate = &candidates[name];
            let filename = if counts[&candidate.to_lowercase()] == 1 {
                candidate.clone()
            } else {
                let mut counter = 0;
                loop {
                    let candidate = format!(
                        "{}--{}-{counter}.json",
                        util::slug(name),
                        &util::hash(name.as_bytes())[..16]
                    );
                    if !reserved.contains(&candidate.to_lowercase())
                        && !used.contains(&candidate.to_lowercase())
                    {
                        break candidate;
                    }
                    counter += 1;
                }
            };
            ensure!(
                used.insert(filename.to_lowercase()),
                "schema filename collision"
            );
            let path = PathBuf::from("schemas").join(filename);
            schema_index.insert(name.clone(), json!(path.to_string_lossy()));
            files.insert(path, util::json_bytes(schema)?);
        }
    }
    files.insert(
        PathBuf::from("index.json"),
        util::json_bytes(&json!({"operations":index,"schemas":schema_index}))?,
    );
    Ok(files)
}

pub fn empty_like(document: &Value) -> Value {
    json!({"openapi":document["openapi"],"info":document["info"],"paths":{}})
}

pub fn validate_snapshot(document: &Value) -> Result<()> {
    validate(document)?;
    validate_schema_locations(document, document, &mut Vec::new(), &mut BTreeSet::new())?;
    operations(document)?;
    Ok(())
}

fn validate_schema_locations(
    document: &Value,
    value: &Value,
    path: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) -> Result<()> {
    let named_map = path.len() >= 2
        && [
            "properties",
            "patternProperties",
            "schemas",
            "$defs",
            "definitions",
        ]
        .contains(&path[path.len() - 2].as_str());
    let schema = path.last().is_some_and(|key| key == "schema") && !named_map;
    let component = path.len() >= 2 && path[path.len() - 2] == "schemas";
    if schema || component {
        return validate_schema(document, value, &pointer(path), seen);
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if !literal(path, key, child) {
                    path.push(key.clone());
                    validate_schema_locations(document, child, path, seen)?;
                    path.pop();
                }
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                path.push(index.to_string());
                validate_schema_locations(document, child, path, seen)?;
                path.pop();
            }
        }
        _ => {}
    }
    Ok(())
}

/// Validate common schema structure locally, independently of the engine.
fn validate_schema(
    document: &Value,
    value: &Value,
    at: &str,
    seen: &mut BTreeSet<String>,
) -> Result<()> {
    if !seen.insert(at.to_owned()) {
        return Ok(());
    }
    let is31 = document["openapi"]
        .as_str()
        .is_some_and(|v| v.starts_with("3.1."));
    if value.is_boolean() {
        ensure!(is31, "boolean schemas require OpenAPI 3.1 at {at}");
        return Ok(());
    }
    let map = value
        .as_object()
        .with_context(|| format!("schema at {at} must be an object"))?;
    if let Some(reference) = map.get("$ref") {
        let reference = reference.as_str().context("schema $ref must be a string")?;
        let target = reference
            .strip_prefix('#')
            .context("unbundled schema reference")?;
        validate_schema(
            document,
            document
                .pointer(target)
                .context("missing schema reference")?,
            target,
            seen,
        )?;
    }
    if let Some(kind) = map.get("type") {
        let types: Vec<&Value> = if let Some(types) = kind.as_array() {
            ensure!(
                is31 && !types.is_empty(),
                "schema type arrays require OpenAPI 3.1 and cannot be empty at {at}"
            );
            types.iter().collect()
        } else {
            vec![kind]
        };
        let mut unique = BTreeSet::new();
        for kind in types {
            let kind = kind
                .as_str()
                .with_context(|| format!("schema type must be a string at {at}"))?;
            ensure!(
                ["string", "number", "integer", "boolean", "object", "array"].contains(&kind)
                    || (is31 && kind == "null"),
                "invalid schema type {kind:?} at {at}"
            );
            ensure!(unique.insert(kind), "duplicate schema type at {at}");
        }
    }
    if let Some(required) = map.get("required") {
        let required = required
            .as_array()
            .with_context(|| format!("schema required must be an array at {at}"))?;
        let mut unique = BTreeSet::new();
        for name in required {
            let name = name
                .as_str()
                .with_context(|| format!("schema required entries must be strings at {at}"))?;
            ensure!(unique.insert(name), "duplicate required property at {at}");
        }
    }
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(children) = map.get(key) {
            let children = children
                .as_object()
                .with_context(|| format!("schema {key} must be an object at {at}"))?;
            for (name, child) in children {
                validate_schema(
                    document,
                    child,
                    &format!("{at}/{key}/{}", util::escape_pointer(name)),
                    seen,
                )?;
            }
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = map.get(key) {
            let children = children
                .as_array()
                .with_context(|| format!("schema {key} must be an array at {at}"))?;
            ensure!(
                key == "prefixItems" || !children.is_empty(),
                "schema {key} cannot be empty at {at}"
            );
            for (index, child) in children.iter().enumerate() {
                validate_schema(document, child, &format!("{at}/{key}/{index}"), seen)?;
            }
        }
    }
    for key in [
        "items",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
        "additionalProperties",
        "unevaluatedProperties",
        "unevaluatedItems",
    ] {
        if let Some(child) = map.get(key) {
            if child.is_boolean() && key == "additionalProperties" {
                continue;
            }
            validate_schema(document, child, &format!("{at}/{key}"), seen)?;
        }
    }
    for key in [
        "nullable",
        "readOnly",
        "writeOnly",
        "deprecated",
        "uniqueItems",
    ] {
        if let Some(flag) = map.get(key) {
            ensure!(flag.is_boolean(), "schema {key} must be boolean at {at}");
        }
    }
    for key in ["format", "pattern", "description", "title"] {
        if let Some(text) = map.get(key) {
            ensure!(text.is_string(), "schema {key} must be a string at {at}");
        }
    }
    if let Some(variants) = map.get("enum") {
        ensure!(variants.is_array(), "schema enum must be an array at {at}");
    }
    Ok(())
}
