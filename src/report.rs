use crate::{
    compare::{Change, Comparison},
    extract::Documents,
    openapi,
    proposal::Drift,
    store, util,
};
use anyhow::Result;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Format {
    Markdown,
    Txt,
    Html,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    pub operations: BTreeMap<String, openapi::Operation>,
    pub schemas: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub format_version: u32,
    pub tool_version: String,
    pub baseline: String,
    pub baseline_snapshot: String,
    pub current_snapshot: String,
    pub comparison: Comparison,
    pub services: BTreeMap<String, Inventory>,
    pub proposals: Vec<Drift>,
    pub limitation: String,
}

pub fn build(
    baseline: &str,
    before: &Documents,
    current: &Documents,
    comparison: Comparison,
    proposals: Vec<Drift>,
) -> Result<Report> {
    build_inner(baseline, before, current, comparison, proposals, true)
}

pub fn build_changes(
    baseline: &str,
    before: &Documents,
    current: &Documents,
    comparison: Comparison,
    proposals: Vec<Drift>,
) -> Result<Report> {
    build_inner(baseline, before, current, comparison, proposals, false)
}

fn build_inner(
    baseline: &str,
    before: &Documents,
    current: &Documents,
    comparison: Comparison,
    proposals: Vec<Drift>,
    include_inventory: bool,
) -> Result<Report> {
    let services = current
        .iter()
        .filter(|_| include_inventory)
        .map(|(service, doc)| {
            Ok((
                service.clone(),
                Inventory {
                    operations: openapi::operations(doc)?,
                    schemas: doc
                        .pointer("/components/schemas")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                },
            ))
        })
        .collect::<Result<_>>()?;
    Ok(Report {format_version:1, tool_version:env!("CARGO_PKG_VERSION").into(), baseline:baseline.into(),
        baseline_snapshot:store::snapshot_id(before)?, current_snapshot:store::snapshot_id(current)?, comparison,services,proposals,
        limitation:"Compatibility analysis describes the declared OpenAPI contract. It does not verify runtime behavior or discover consumers.".into()})
}

fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn md(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if "\\`*_{}[]<>()#+-.!|".contains(c) {
                vec!['\\', c]
            } else if c == '\n' || c == '\r' {
                vec![' ']
            } else {
                vec![c]
            }
        })
        .collect()
}
fn code(value: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string_pretty(value)?;
    let longest = json.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    Ok(format!("{fence}json\n{json}\n{fence}\n"))
}

pub fn render(report: &Report, format: Format) -> Result<String> {
    match format {
        Format::Json => Ok(String::from_utf8(util::json_bytes(report)?)?),
        Format::Markdown => markdown(report),
        Format::Txt => text(report),
        Format::Html => html_report(report),
    }
}

fn markdown(report: &Report) -> Result<String> {
    let mut out = format!(
        "# API contract report\n\nBaseline: {} (`{}`)\n\nCurrent snapshot: `{}`\n\nTool: contracts {}; compatibility engine: {}\n\n{} breaking findings; {} total compatibility findings; {} structural/documentation changes.\n\n{}\n",
        md(&report.baseline),
        report.baseline_snapshot,
        report.current_snapshot,
        md(&report.tool_version),
        md(report
            .comparison
            .compatibility_engine
            .as_deref()
            .unwrap_or("not requested")),
        report.comparison.breaking(),
        report.comparison.findings.len(),
        report.comparison.changes.len(),
        md(&report.limitation)
    );
    out.push_str("\n## Compatibility findings\n");
    for finding in &report.comparison.findings {
        out.push_str(&format!(
            "\n### {} · {}\n\n{}\n",
            md(&finding.service),
            md(&finding.classification),
            code(&finding.detail)?
        ));
    }
    out.push_str("\n## Complete changes\n");
    for change in &report.comparison.changes {
        out.push_str(&format!(
            "\n### {} · {}\n\n{}\n",
            md(&change.service),
            md(&change.pointer),
            code(change)?
        ));
    }
    out.push_str("\n## Service inventory\n");
    for (name, inventory) in &report.services {
        out.push_str(&format!("\n### {}\n", md(name)));
        for (identity, operation) in &inventory.operations {
            out.push_str(&format!(
                "\n#### {}\n\n{}\n",
                md(identity),
                code(operation)?
            ));
        }
        out.push_str(&format!(
            "\n#### Reusable schemas\n\n{}\n",
            code(&inventory.schemas)?
        ));
    }
    out.push_str("\n## Open proposals\n");
    for proposal in &report.proposals {
        out.push_str(&format!(
            "\n### {}\n\nDesired → observed drift ({} changes):\n\n{}\n",
            md(&proposal.name),
            proposal.changes.len(),
            code(proposal)?
        ));
    }
    Ok(out)
}

fn text(report: &Report) -> Result<String> {
    let mut out = format!(
        "API CONTRACT REPORT\nBaseline: {} ({})\nCurrent: {}\nTool: contracts {}\nCompatibility engine: {}\nBreaking: {}; findings: {}; changes: {}\n{}\n\nCOMPATIBILITY FINDINGS\n",
        report.baseline,
        report.baseline_snapshot,
        report.current_snapshot,
        report.tool_version,
        report
            .comparison
            .compatibility_engine
            .as_deref()
            .unwrap_or("not requested"),
        report.comparison.breaking(),
        report.comparison.findings.len(),
        report.comparison.changes.len(),
        report.limitation
    );
    for finding in &report.comparison.findings {
        out.push_str(&format!("{}\n", serde_json::to_string_pretty(finding)?));
    }
    out.push_str("\nCOMPLETE CHANGES\n");
    for change in &report.comparison.changes {
        out.push_str(&format!("{}\n", serde_json::to_string_pretty(change)?));
    }
    out.push_str("\nSERVICE INVENTORY\n");
    out.push_str(&serde_json::to_string_pretty(&report.services)?);
    out.push_str("\n\nOPEN PROPOSALS (desired -> observed)\n");
    out.push_str(&serde_json::to_string_pretty(&report.proposals)?);
    out.push('\n');
    Ok(out)
}

fn html_report(report: &Report) -> Result<String> {
    let mut out = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>API contract report</title><style>body{{font:16px system-ui;max-width:1100px;margin:40px auto;padding:0 24px;color:#172033;background:#f8fafc}}h1,h2{{letter-spacing:-.03em}}section,details{{background:white;border:1px solid #dbe2ea;border-radius:10px;padding:16px;margin:12px 0}}summary{{cursor:pointer;font-weight:600}}pre{{overflow:auto;background:#f1f5f9;padding:16px;border-radius:6px;font-size:13px}}.breaking{{color:#b42318}}code{{overflow-wrap:anywhere}}</style></head><body><h1>API contract report</h1><p>Baseline: {} · <code>{}</code></p><p>Current: <code>{}</code></p><p>contracts {} · {}</p><p class=\"breaking\">{} breaking findings</p><p>{}</p>",
        html(&report.baseline),
        html(&report.baseline_snapshot),
        html(&report.current_snapshot),
        html(&report.tool_version),
        html(
            report
                .comparison
                .compatibility_engine
                .as_deref()
                .unwrap_or("compatibility not requested")
        ),
        report.comparison.breaking(),
        html(&report.limitation)
    );
    out.push_str("<h2>Compatibility findings</h2>");
    for finding in &report.comparison.findings {
        out.push_str(&format!(
            "<details><summary>{} · {} · {}</summary><pre>{}</pre></details>",
            html(&finding.service),
            html(&finding.classification),
            html(
                finding
                    .detail
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("Finding")
            ),
            html(&serde_json::to_string_pretty(&finding.detail)?)
        ));
    }
    out.push_str("<h2>Complete changes</h2>");
    for change in &report.comparison.changes {
        out.push_str(&html_change(change)?);
    }
    out.push_str("<h2>Service inventory</h2>");
    for (name, inventory) in &report.services {
        out.push_str(&format!("<h3>{}</h3>", html(name)));
        for (identity, operation) in &inventory.operations {
            out.push_str(&format!(
                "<details><summary>{}</summary><pre>{}</pre></details>",
                html(identity),
                html(&serde_json::to_string_pretty(operation)?)
            ));
        }
        out.push_str(&format!(
            "<details><summary>Reusable schemas</summary><pre>{}</pre></details>",
            html(&serde_json::to_string_pretty(&inventory.schemas)?)
        ));
    }
    out.push_str("<h2>Open proposals</h2>");
    for proposal in &report.proposals {
        out.push_str(&format!(
            "<details><summary>{} · {} drift changes</summary><pre>{}</pre></details>",
            html(&proposal.name),
            proposal.changes.len(),
            html(&serde_json::to_string_pretty(proposal)?)
        ));
    }
    out.push_str("</body></html>\n");
    Ok(out)
}
fn html_change(change: &Change) -> Result<String> {
    Ok(format!(
        "<details><summary>{} · {} · {}</summary><pre>{}</pre></details>",
        html(&change.service),
        html(&change.kind),
        html(&change.pointer),
        html(&serde_json::to_string_pretty(change)?)
    ))
}
