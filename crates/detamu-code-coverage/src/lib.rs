//! Test coverage evidence adapter for the Detamu code world model.
//!
//! Reports are parsed before indexing and then applied to reconciled symbols by
//! source range. Detamu consumes coverage artifacts; it never runs test suites.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use detamu_core::{
    AnalysisDiagnostic, DiagnosticSeverity, EvidenceProvenance, Measurement, ObserverProvenance,
};
use detamu_model::{AnalyzerCapability, DerivationError, DeriverDescriptor, ObservationDeriver};
use detamu_model_code::CODE_MODEL_ID;
use quick_xml::{Reader, XmlVersion, events::Event};
use thiserror::Error;

const OBSERVER: &str = "code.coverage";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageFormat {
    Lcov,
    Cobertura,
}

impl CoverageFormat {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Lcov => "lcov",
            Self::Cobertura => "cobertura",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CoverageReport {
    pub format: CoverageFormat,
    pub source: String,
    pub bytes: Vec<u8>,
}

impl CoverageReport {
    pub fn new(
        format: CoverageFormat,
        source: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            format,
            source: source.into(),
            bytes: bytes.into(),
        }
    }

    /// Reads a report and detects LCOV or Cobertura from its contents.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or its format is unknown.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, CoverageError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|error| CoverageError::Read {
            path: path.to_owned(),
            error,
        })?;
        let format = detect_format(path, &bytes)?;
        Ok(Self::new(
            format,
            path.to_string_lossy().into_owned(),
            bytes,
        ))
    }
}

#[derive(Debug, Error)]
pub enum CoverageError {
    #[error("failed to read coverage report {path}: {error}")]
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("coverage report format is not recognized: {0}")]
    UnknownFormat(String),
    #[error("invalid {format} report {report_source}: {message}")]
    Invalid {
        format: &'static str,
        report_source: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Default)]
struct LineCoverage {
    hit: bool,
    branches_covered: u32,
    branches_total: u32,
}

impl LineCoverage {
    fn merge(&mut self, other: Self) {
        self.hit |= other.hit;
        self.branches_covered = self.branches_covered.max(other.branches_covered);
        self.branches_total = self.branches_total.max(other.branches_total);
    }
}

#[derive(Debug, Clone, Default)]
struct FileCoverage {
    lines: BTreeMap<u32, LineCoverage>,
}

impl FileCoverage {
    fn line(&mut self, number: u32, coverage: LineCoverage) {
        self.lines.entry(number).or_default().merge(coverage);
    }

    fn merge(&mut self, other: Self) {
        for (number, coverage) in other.lines {
            self.line(number, coverage);
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodeCoverageDeriver {
    files: BTreeMap<String, FileCoverage>,
    sources: Vec<String>,
    formats: BTreeSet<&'static str>,
}

impl CodeCoverageDeriver {
    /// Parses coverage artifacts into an immutable derivation input.
    ///
    /// # Errors
    ///
    /// Returns an error when any report is malformed.
    pub fn from_reports(
        reports: impl IntoIterator<Item = CoverageReport>,
    ) -> Result<Self, CoverageError> {
        let mut files = BTreeMap::<String, FileCoverage>::new();
        let mut sources = Vec::new();
        let mut formats = BTreeSet::new();
        for report in reports {
            let parsed = match report.format {
                CoverageFormat::Lcov => parse_lcov(&report)?,
                CoverageFormat::Cobertura => parse_cobertura(&report)?,
            };
            for (path, coverage) in parsed {
                files.entry(path).or_default().merge(coverage);
            }
            formats.insert(report.format.as_str());
            sources.push(report.source);
        }
        Ok(Self {
            files,
            sources,
            formats,
        })
    }

    /// Reads and parses coverage reports from disk.
    ///
    /// # Errors
    ///
    /// Returns an error when a report cannot be read, detected, or parsed.
    pub fn from_paths(
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> Result<Self, CoverageError> {
        let reports = paths
            .into_iter()
            .map(CoverageReport::from_path)
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_reports(reports)
    }

    fn file(&self, entity_path: &str) -> Option<(&str, &FileCoverage)> {
        let entity_path = normalize_path(entity_path);
        if let Some((path, coverage)) = self.files.get_key_value(&entity_path) {
            return Some((path, coverage));
        }
        let mut matches = self.files.iter().filter(|(report_path, _)| {
            report_path.ends_with(&format!("/{entity_path}"))
                || entity_path.ends_with(&format!("/{report_path}"))
        });
        let first = matches.next()?;
        matches
            .next()
            .is_none()
            .then_some((first.0.as_str(), first.1))
    }
}

impl ObservationDeriver for CodeCoverageDeriver {
    fn descriptor(&self) -> DeriverDescriptor {
        DeriverDescriptor {
            name: OBSERVER.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            model: detamu_core::ModelId::new(CODE_MODEL_ID),
            capabilities: vec![AnalyzerCapability::Other("test_coverage".to_owned())],
        }
    }

    fn derive(&self, batch: &mut detamu_core::ObservationBatch) -> Result<(), DerivationError> {
        let mut matched_files = BTreeSet::new();
        for observation in &mut batch.entities {
            if observation.entity.model.as_str() != CODE_MODEL_ID {
                continue;
            }
            let Some(path) = string_attribute(&observation.attributes, "file_path") else {
                continue;
            };
            let Some(start) = u32_attribute(&observation.attributes, "line_start") else {
                continue;
            };
            let end = u32_attribute(&observation.attributes, "line_end").unwrap_or(start);
            let Some((report_path, coverage)) = self.file(path) else {
                continue;
            };
            let Some((line_ratio, branch_ratio)) = ratios(coverage, start, end) else {
                continue;
            };
            matched_files.insert(report_path.to_owned());
            observation.measurements.extend([
                coverage_measurement("test.line_coverage", line_ratio),
                coverage_measurement("test.branch_coverage", branch_ratio),
            ]);
        }
        for path in self.files.keys() {
            if !matched_files.contains(path) {
                batch.diagnostics.push(AnalysisDiagnostic {
                    severity: DiagnosticSeverity::Info,
                    observer: OBSERVER.to_owned(),
                    message: "coverage file did not match any measured code entity".to_owned(),
                    scope: Some(path.clone()),
                });
            }
        }
        batch.provenance.push(ObserverProvenance {
            observer: OBSERVER.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            configuration_digest: Some(format!(
                "{}-range-v1",
                self.formats.iter().copied().collect::<Vec<_>>().join("+")
            )),
            source: Some(self.sources.join(",")),
        });
        Ok(())
    }
}

fn coverage_measurement(name: &str, value: f64) -> Measurement {
    Measurement {
        name: name.to_owned(),
        value,
        unit: Some("ratio".to_owned()),
        evidence: Some(EvidenceProvenance {
            observer: OBSERVER.to_owned(),
            confidence: 1.0,
        }),
    }
}

fn ratios(coverage: &FileCoverage, start: u32, end: u32) -> Option<(f64, f64)> {
    let lines = coverage
        .lines
        .range(start..=end)
        .map(|(_, coverage)| *coverage)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let covered_lines = lines
        .iter()
        .filter(|coverage| coverage.hit)
        .fold(0_u32, |count, _| count.saturating_add(1));
    let lines_total = u32::try_from(lines.len()).ok()?;
    let branches_total = lines
        .iter()
        .map(|coverage| coverage.branches_total)
        .sum::<u32>();
    let branches_covered = lines
        .iter()
        .map(|coverage| coverage.branches_covered)
        .sum::<u32>();
    let line_ratio = f64::from(covered_lines) / f64::from(lines_total);
    let branch_ratio = if branches_total == 0 {
        1.0
    } else {
        f64::from(branches_covered) / f64::from(branches_total)
    };
    Some((line_ratio, branch_ratio))
}

fn parse_lcov(report: &CoverageReport) -> Result<BTreeMap<String, FileCoverage>, CoverageError> {
    let text = std::str::from_utf8(&report.bytes)
        .map_err(|error| invalid(report, format!("report is not UTF-8: {error}")))?;
    let mut files = BTreeMap::<String, FileCoverage>::new();
    let mut current = None::<String>;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            current = Some(normalize_path(path));
        } else if let Some(value) = line.strip_prefix("DA:") {
            let path = current
                .as_ref()
                .ok_or_else(|| invalid(report, "DA record appears before SF"))?;
            let mut fields = value.split(',');
            let number = parse_u32(report, fields.next(), "line number")?;
            let hits = parse_u64(report, fields.next(), "line hits")?;
            files.entry(path.clone()).or_default().line(
                number,
                LineCoverage {
                    hit: hits > 0,
                    ..LineCoverage::default()
                },
            );
        } else if let Some(value) = line.strip_prefix("BRDA:") {
            let path = current
                .as_ref()
                .ok_or_else(|| invalid(report, "BRDA record appears before SF"))?;
            let mut fields = value.split(',');
            let number = parse_u32(report, fields.next(), "branch line number")?;
            let _block = fields.next();
            let _branch = fields.next();
            let taken = fields
                .next()
                .ok_or_else(|| invalid(report, "BRDA record lacks taken count"))?;
            let line = files
                .entry(path.clone())
                .or_default()
                .lines
                .entry(number)
                .or_default();
            line.branches_total = line.branches_total.saturating_add(1);
            if taken != "-" && taken.parse::<u64>().is_ok_and(|hits| hits > 0) {
                line.branches_covered = line.branches_covered.saturating_add(1);
            }
        } else if line == "end_of_record" {
            current = None;
        }
    }
    Ok(files)
}

fn parse_cobertura(
    report: &CoverageReport,
) -> Result<BTreeMap<String, FileCoverage>, CoverageError> {
    let mut reader = Reader::from_reader(report.bytes.as_slice());
    reader.config_mut().trim_text(true);
    let mut files = BTreeMap::<String, FileCoverage>::new();
    let mut current = None::<String>;
    loop {
        match reader
            .read_event()
            .map_err(|error| invalid(report, error.to_string()))?
        {
            Event::Start(element) => match element.local_name().as_ref() {
                b"class" => current = attribute(&reader, report, &element, b"filename")?,
                b"line" => {
                    parse_cobertura_line(
                        &reader,
                        report,
                        &element,
                        current.as_deref(),
                        &mut files,
                    )?;
                }
                _ => {}
            },
            Event::Empty(element) if element.local_name().as_ref() == b"line" => {
                parse_cobertura_line(&reader, report, &element, current.as_deref(), &mut files)?;
            }
            Event::End(element) if element.local_name().as_ref() == b"class" => current = None,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(files)
}

fn parse_cobertura_line(
    reader: &Reader<&[u8]>,
    report: &CoverageReport,
    element: &quick_xml::events::BytesStart<'_>,
    current: Option<&str>,
    files: &mut BTreeMap<String, FileCoverage>,
) -> Result<(), CoverageError> {
    let Some(path) = current else {
        return Ok(());
    };
    let number = parse_u32(
        report,
        attribute(reader, report, element, b"number")?.as_deref(),
        "line number",
    )?;
    let hits = parse_u64(
        report,
        attribute(reader, report, element, b"hits")?.as_deref(),
        "line hits",
    )?;
    let (branches_covered, branches_total) =
        attribute(reader, report, element, b"condition-coverage")?
            .as_deref()
            .map_or(Ok((0, 0)), |value| parse_condition_coverage(report, value))?;
    files.entry(normalize_path(path)).or_default().line(
        number,
        LineCoverage {
            hit: hits > 0,
            branches_covered,
            branches_total,
        },
    );
    Ok(())
}

fn attribute(
    reader: &Reader<&[u8]>,
    report: &CoverageReport,
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>, CoverageError> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| invalid(report, error.to_string()))?;
        if attribute.key.local_name().as_ref() == name {
            return attribute
                .decoded_and_normalized_value(XmlVersion::default(), reader.decoder())
                .map(|value| Some(value.into_owned()))
                .map_err(|error| invalid(report, error.to_string()));
        }
    }
    Ok(None)
}

fn parse_condition_coverage(
    report: &CoverageReport,
    value: &str,
) -> Result<(u32, u32), CoverageError> {
    let counts = value
        .split_once('(')
        .and_then(|(_, counts)| counts.strip_suffix(')'))
        .ok_or_else(|| invalid(report, format!("invalid condition coverage: {value}")))?;
    let (covered, total) = counts
        .split_once('/')
        .ok_or_else(|| invalid(report, format!("invalid condition coverage: {value}")))?;
    Ok((
        parse_u32(report, Some(covered), "covered branches")?,
        parse_u32(report, Some(total), "total branches")?,
    ))
}

fn detect_format(path: &Path, bytes: &[u8]) -> Result<CoverageFormat, CoverageError> {
    let text = std::str::from_utf8(bytes).unwrap_or_default().trim_start();
    if text.starts_with("TN:") || text.starts_with("SF:") {
        Ok(CoverageFormat::Lcov)
    } else if text.starts_with('<') && text.contains("<coverage") {
        Ok(CoverageFormat::Cobertura)
    } else {
        Err(CoverageError::UnknownFormat(
            path.to_string_lossy().into_owned(),
        ))
    }
}

fn parse_u32(
    report: &CoverageReport,
    value: Option<&str>,
    field: &str,
) -> Result<u32, CoverageError> {
    value
        .ok_or_else(|| invalid(report, format!("missing {field}")))?
        .trim()
        .parse()
        .map_err(|error| invalid(report, format!("invalid {field}: {error}")))
}

fn parse_u64(
    report: &CoverageReport,
    value: Option<&str>,
    field: &str,
) -> Result<u64, CoverageError> {
    value
        .ok_or_else(|| invalid(report, format!("missing {field}")))?
        .trim()
        .parse()
        .map_err(|error| invalid(report, format!("invalid {field}: {error}")))
}

fn invalid(report: &CoverageReport, message: impl Into<String>) -> CoverageError {
    CoverageError::Invalid {
        format: report.format.as_str(),
        report_source: report.source.clone(),
        message: message.into(),
    }
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn string_attribute<'a>(attributes: &'a detamu_core::Attributes, name: &str) -> Option<&'a str> {
    attributes.get(name)?.as_str()
}

fn u32_attribute(attributes: &detamu_core::Attributes, name: &str) -> Option<u32> {
    u32::try_from(attributes.get(name)?.as_u64()?).ok()
}

#[cfg(test)]
mod tests {
    use detamu_core::{
        AnalysisCoverage, Attributes, Entity, EntityId, EntityObservation, ModelId,
        ObservationBatch, SnapshotId, SnapshotVersion, WorldId,
    };
    use detamu_model::ScoringModel;
    use detamu_model_code::{AvecCodeScorer, NodeMetrics};
    use serde_json::json;

    use super::*;

    #[test]
    fn derives_symbol_ratios_from_lcov_lines_and_branches() {
        let report = CoverageReport::new(
            CoverageFormat::Lcov,
            "lcov.info",
            b"TN:\nSF:/workspace/src/lib.rs\nDA:10,1\nDA:11,0\nDA:12,2\nBRDA:11,0,0,1\nBRDA:11,0,1,0\nend_of_record\n",
        );
        let deriver = CodeCoverageDeriver::from_reports([report]).expect("parse LCOV");
        let mut batch = batch_with_symbol("src/lib.rs", 10, 13);

        deriver.derive(&mut batch).expect("derive coverage");

        assert_ratio(&batch, "test.line_coverage", 2.0 / 3.0);
        assert_ratio(&batch, "test.branch_coverage", 0.5);
        assert_eq!(batch.provenance[0].observer, OBSERVER);
        assert!(batch.diagnostics.is_empty());
    }

    #[test]
    fn derives_symbol_ratios_from_cobertura_and_normalizes_paths() {
        let report = CoverageReport::new(
            CoverageFormat::Cobertura,
            "coverage.xml",
            br#"<?xml version="1.0"?>
<coverage>
  <packages>
    <package name="fixture">
      <classes>
        <class name="Service" filename="src/service.cs">
          <lines>
            <line number="5" hits="1" branch="true" condition-coverage="50% (1/2)"/>
            <line number="6" hits="0" branch="false"/>
          </lines>
        </class>
      </classes>
    </package>
  </packages>
</coverage>"#,
        );
        let deriver = CodeCoverageDeriver::from_reports([report]).expect("parse Cobertura");
        let mut batch = batch_with_symbol(r"src\service.cs", 5, 6);

        deriver.derive(&mut batch).expect("derive coverage");

        assert_ratio(&batch, "test.line_coverage", 0.5);
        assert_ratio(&batch, "test.branch_coverage", 0.5);
    }

    #[test]
    fn rejects_malformed_coverage_records() {
        let report = CoverageReport::new(
            CoverageFormat::Lcov,
            "broken.info",
            b"SF:src/lib.rs\nDA:not-a-line,1\n",
        );

        assert!(CodeCoverageDeriver::from_reports([report]).is_err());
    }

    #[test]
    fn coverage_evidence_unlocks_avec_scoring() {
        let report = CoverageReport::new(
            CoverageFormat::Lcov,
            "lcov.info",
            b"SF:src/lib.rs\nDA:10,1\nDA:11,1\nend_of_record\n",
        );
        let deriver = CodeCoverageDeriver::from_reports([report]).expect("parse LCOV");
        let mut batch = batch_with_symbol("src/lib.rs", 10, 11);
        batch.entities[0].measurements = NodeMetrics {
            lines_of_code: 2,
            cyclomatic_complexity: 1,
            parameters: 1,
            incoming_edges: 0,
            outgoing_edges: 0,
            git_total_commits: 1,
            git_contributors: 1,
            git_average_days_between_changes: 10.0,
            test_line_coverage: 0.0,
            test_branch_coverage: 0.0,
        }
        .measurements()
        .into_iter()
        .filter(|measurement| !measurement.name.starts_with("test."))
        .collect();

        assert!(batch.entities[0].scores.is_empty());
        deriver.derive(&mut batch).expect("derive coverage");
        AvecCodeScorer::default()
            .score(&mut batch)
            .expect("score coverage-enriched symbol");

        assert_eq!(batch.entities[0].scores.len(), 4);
    }

    fn batch_with_symbol(path: &str, start: u32, end: u32) -> ObservationBatch {
        let snapshot = SnapshotId::new(
            WorldId::new("code.repository:fixture"),
            SnapshotVersion::new("abc"),
        );
        let mut attributes = Attributes::new();
        attributes.insert("file_path".to_owned(), json!(path));
        attributes.insert("line_start".to_owned(), json!(start));
        attributes.insert("line_end".to_owned(), json!(end));
        let mut batch = ObservationBatch::empty(snapshot.clone());
        batch.coverage = AnalysisCoverage::Partial;
        batch.entities.push(EntityObservation {
            snapshot,
            entity: Entity {
                id: EntityId::new("node_fixture"),
                model: ModelId::new(CODE_MODEL_ID),
                kind: "function".to_owned(),
                label: "fixture".to_owned(),
            },
            attributes,
            measurements: Vec::new(),
            scores: Vec::new(),
        });
        batch
    }

    fn assert_ratio(batch: &ObservationBatch, name: &str, expected: f64) {
        let measurement = batch.entities[0]
            .measurements
            .iter()
            .find(|measurement| measurement.name == name)
            .expect("coverage measurement");
        assert!((measurement.value - expected).abs() < f64::EPSILON);
        assert_eq!(
            measurement
                .evidence
                .as_ref()
                .map(|evidence| evidence.observer.as_str()),
            Some(OBSERVER)
        );
    }
}
