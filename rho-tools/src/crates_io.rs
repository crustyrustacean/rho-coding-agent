use reqwest::Client;
use serde::Deserialize;
use url::Url;

use crate::error::{ToolError, ToolResult};
use rho_core::{
    CancellationToken, RhoError, Tool, ToolName, ToolOutcome, ToolResult as ToolOutcomeResult,
    ToolRisk,
};
use tracing::{debug, warn};

#[derive(Clone, Debug, Deserialize)]
pub struct CratesIoSearchResponse {
    /// The list of crates returned by the search.
    pub crates: Vec<CrateSummary>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CrateSummary {
    /// The crates.io crate ID.
    pub id: String,
    /// The crate name.
    pub name: String,
    /// A brief description of the crate.
    pub description: Option<String>,
    /// URL to the crate's documentation.
    pub documentation: Option<String>,
    /// Total number of downloads.
    pub downloads: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CratesIoDetailResponse {
    /// The detailed crate information.
    #[serde(rename = "crate")]
    pub detail: CrateDetails,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CrateDetails {
    pub name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub documentation: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    pub downloads: u64,
    pub max_version: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CrateDependency {
    pub crate_id: String,
    pub req: String,
    pub kind: String,
    pub optional: bool,
    pub default_features: bool,
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CratesIoDepsResponse {
    /// The list of dependencies for a specific version.
    pub dependencies: Vec<CrateDependency>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CratesIoVersionsResponse {
    /// The list of versions available for the crate.
    pub versions: Vec<CrateVersion>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CrateVersion {
    /// The version number.
    pub num: String,
    /// Whether the version has been yanked.
    pub yanked: bool,
}

#[derive(Clone, Debug)]
pub struct CratesIoClient {
    /// The HTTP client used for API requests.
    http_client: Client,
}

impl CratesIoClient {
    /// Create a new `CratesIoClient`.
    ///
    /// # Panics
    ///
    /// Panics if the HTTP client cannot be built (e.g. TLS initialization failure).
    pub fn new() -> Self {
        Self {
            http_client: Client::builder()
                .user_agent(
                    "rho-coding-agent (https://github.com/crustyrustacean/rho-coding-agent)",
                )
                .build()
                .expect("failed to build reqwest Client"),
        }
    }

    /// Search crates.io for crates matching the given query.
    ///
    /// # Errors
    ///
    /// Returns `ToolError::Http` if the request fails at the transport level,
    /// or `ToolError::ApiError` if the API returns a non-success status or
    /// the response body cannot be parsed as JSON.
    pub async fn search_crates(&self, query: &str) -> ToolResult<Vec<CrateSummary>> {
        let endpoint = search_url(query);
        debug!(query = %query, "crates.io: searching crates");
        let response: CratesIoSearchResponse = self.get_json(&endpoint).await?;
        debug!(
            results = response.crates.len(),
            "crates.io: search completed"
        );
        Ok(response.crates)
    }

    /// Fetch detailed metadata for a specific crate.
    ///
    /// # Errors
    ///
    /// Returns `ToolError::Http` if the request fails at the transport level,
    /// or `ToolError::ApiError` if the API returns a non-success status or
    /// the response body cannot be parsed as JSON.
    pub async fn info_crate(&self, name: &str) -> ToolResult<CrateDetails> {
        let endpoint = format!("https://crates.io/api/v1/crates/{name}");
        debug!(name = %name, "crates.io: fetching crate info");
        let response: CratesIoDetailResponse = self.get_json(&endpoint).await?;
        Ok(response.detail)
    }

    /// Fetch the version history for a specific crate.
    ///
    /// # Errors
    ///
    /// Returns `ToolError::Http` if the request fails at the transport level,
    /// or `ToolError::ApiError` if the API returns a non-success status or
    /// the response body cannot be parsed as JSON.
    pub async fn versions_crate(&self, name: &str) -> ToolResult<Vec<CrateVersion>> {
        let endpoint = format!("https://crates.io/api/v1/crates/{name}/versions");
        let response: CratesIoVersionsResponse = self.get_json(&endpoint).await?;
        Ok(response.versions)
    }

    /// Fetch the dependencies for the latest version of a specific crate.
    ///
    /// # Errors
    ///
    /// Returns `ToolError::Http` if the request fails at the transport level,
    /// or `ToolError::ApiError` if the API returns a non-success status or
    /// the response body cannot be parsed as JSON.
    pub async fn deps_crate(&self, name: &str) -> ToolResult<Vec<CrateDependency>> {
        let details = self.info_crate(name).await?;
        let version = &details.max_version;
        let endpoint = format!("https://crates.io/api/v1/crates/{name}/{version}/dependencies");
        let response: CratesIoDepsResponse = self.get_json(&endpoint).await?;
        Ok(response.dependencies)
    }

    /// Fetch a JSON response from the crates.io API.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> ToolResult<T> {
        let url = Url::parse(endpoint).map_err(|e| ToolError::ApiError {
            status: 0,
            message: format!("invalid URL: {e}"),
        })?;

        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::Http { source: e })?;

        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            warn!(
                url = %endpoint,
                status = %status.as_u16(),
                "crates.io API returned error"
            );
            return Err(ToolError::ApiError {
                status: status.as_u16(),
                message: text,
            });
        }

        // Read the body as text then deserialize, so parse errors can
        // include a snippet of the response for diagnostics.
        let body_text = response
            .text()
            .await
            .map_err(|e| ToolError::Http { source: e })?;

        let body: T = serde_json::from_str(&body_text).map_err(|e| ToolError::ApiError {
            status: status.as_u16(),
            message: format!(
                "JSON parse error: {e}; body: {}",
                body_text.chars().take(500).collect::<String>()
            ),
        })?;

        Ok(body)
    }
}

impl Default for CratesIoClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the crates.io search URL for a given query.
fn search_url(query: &str) -> String {
    format!("https://crates.io/api/v1/crates?q={query}")
}

// ── Tool wrapper ──────────────────────────────────────────────────────────────

/// Tool for researching crates on crates.io.
///
/// Supports four operations:
/// - `search` — search by keyword, ranked by downloads
/// - `info` — crate metadata (name, version, downloads, license, repo, docs, keywords)
/// - `versions` — version history with yanked status
/// - `deps` — dependency tree grouped by kind (normal, dev, build)
#[derive(Clone, Debug)]
pub struct CratesIoLookup {
    /// The underlying crates.io API client.
    client: CratesIoClient,
}

impl CratesIoLookup {
    /// Create a new `CratesIoLookup` tool.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: CratesIoClient::new(),
        }
    }
}

impl Default for CratesIoLookup {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for CratesIoLookup {
    fn name(&self) -> ToolName {
        ToolName::from("crates_io_lookup")
    }

    fn description(&self) -> &str {
        "Look up crates on crates.io. Supports four operations: \
         `search` (by keyword, ranked by downloads), \
         `info` (metadata: name, version, downloads, license, repo, docs, keywords), \
         `versions` (version history with yanked status), \
         `deps` (dependency tree by kind)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["info", "search", "versions", "deps"],
                    "description": "The operation to perform"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (required when operation is 'search')"
                },
                "crate_name": {
                    "type": "string",
                    "description": "Crate name (required when operation is 'info', 'versions', or 'deps')"
                }
            },
            "required": ["operation"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> std::result::Result<ToolOutcome, RhoError> {
        let operation = arguments
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RhoError::Tool("missing required argument `operation`".to_string()))?;

        match operation {
            "search" => {
                let query = arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RhoError::Tool("missing required argument `query` for search".to_string())
                    })?;
                let results = self.client.search_crates(query).await?;
                let output = format_crate_summaries(&results);
                Ok(ToolOutcome::Immediate(ToolOutcomeResult::success(output)))
            }
            "info" => {
                let name = arguments
                    .get("crate_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RhoError::Tool(
                            "missing required argument `crate_name` for info".to_string(),
                        )
                    })?;
                let details = self.client.info_crate(name).await?;
                let output = format_crate_details(&details);
                Ok(ToolOutcome::Immediate(ToolOutcomeResult::success(output)))
            }
            "versions" => {
                let name = arguments
                    .get("crate_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RhoError::Tool(
                            "missing required argument `crate_name` for versions".to_string(),
                        )
                    })?;
                let versions = self.client.versions_crate(name).await?;
                let output = format_crate_versions(&versions);
                Ok(ToolOutcome::Immediate(ToolOutcomeResult::success(output)))
            }
            "deps" => {
                let name = arguments
                    .get("crate_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RhoError::Tool(
                            "missing required argument `crate_name` for deps".to_string(),
                        )
                    })?;
                let deps = self.client.deps_crate(name).await?;
                let output = format_crate_deps(&deps);
                Ok(ToolOutcome::Immediate(ToolOutcomeResult::success(output)))
            }
            other => Err(RhoError::Tool(format!(
                "unknown operation `{other}`; expected one of: info, search, versions, deps"
            ))),
        }
    }
}

// ── Output formatting ─────────────────────────────────────────────────────────

/// Format search results wrapped in `<crate>` tags.
fn format_crate_summaries(crates: &[CrateSummary]) -> String {
    let mut parts = Vec::new();
    for c in crates {
        parts.push(format!(
            "<crate name=\"{}\" downloads=\"{}\">\n  description: {}\n  docs: {}\n</crate>",
            c.name,
            c.downloads,
            c.description.as_deref().unwrap_or("none"),
            c.documentation.as_deref().unwrap_or("none"),
        ));
    }
    parts.join("\n\n")
}

/// Format crate details wrapped in `<crate>` tags.
fn format_crate_details(details: &CrateDetails) -> String {
    format!(
        "<crate name=\"{}\" version=\"{}\" downloads=\"{}\">\n  description: {}\n  license: {}\n  repository: {}\n  homepage: {}\n  docs: {}\n  keywords: {}\n</crate>",
        details.name,
        details.max_version,
        details.downloads,
        details.description.as_deref().unwrap_or("none"),
        details.license.as_deref().unwrap_or("none"),
        details.repository.as_deref().unwrap_or("none"),
        details.homepage.as_deref().unwrap_or("none"),
        details.documentation.as_deref().unwrap_or("none"),
        details.keywords.join(", "),
    )
}

/// Format versions wrapped in `<crate>` tags.
fn format_crate_versions(versions: &[CrateVersion]) -> String {
    let mut parts = Vec::new();
    for v in versions {
        let status = if v.yanked { "yanked" } else { "available" };
        parts.push(format!("<version num=\"{}\" status=\"{status}\" />", v.num));
    }
    parts.join("\n")
}

/// Format dependencies wrapped in `<crate>` tags.
fn format_crate_deps(deps: &[CrateDependency]) -> String {
    let mut parts = Vec::new();
    for d in deps {
        let optional = if d.optional { " optional" } else { "" };
        let features = if d.features.is_empty() {
            String::new()
        } else {
            format!(" features=\"{}\"", d.features.join(", "))
        };
        parts.push(format!(
            "<dependency name=\"{}\" req=\"{}\" kind=\"{}\"{}{} />",
            d.crate_id, d.req, d.kind, optional, features
        ));
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {

    use super::*;

    // ── Tool trait ─────────────────────────────────────────────────────────

    #[test]
    fn tool_name() {
        let tool = CratesIoLookup::new();
        assert_eq!(&*tool.name(), "crates_io_lookup");
    }

    #[test]
    fn tool_risk_is_read() {
        let tool = CratesIoLookup::new();
        assert_eq!(tool.risk(), ToolRisk::Read);
    }

    #[test]
    fn tool_description_is_not_empty() {
        let tool = CratesIoLookup::new();
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("search"));
        assert!(tool.description().contains("info"));
        assert!(tool.description().contains("versions"));
        assert!(tool.description().contains("deps"));
    }

    #[test]
    fn tool_parameters_schema_is_valid() {
        let tool = CratesIoLookup::new();
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["operation"]["enum"].is_array());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::Value::String("operation".to_string()))
        );
    }

    #[test]
    fn format_summaries_empty() {
        let output = format_crate_summaries(&[]);
        assert_eq!(output, "");
    }

    #[test]
    fn format_summaries_one_crate() {
        let crates = vec![CrateSummary {
            id: "serde".to_string(),
            name: "serde".to_string(),
            description: Some("A serialization framework".to_string()),
            documentation: Some("https://docs.rs/serde".to_string()),
            downloads: 100_000_000,
        }];
        let output = format_crate_summaries(&crates);
        assert!(output.contains("<crate name=\"serde\""));
        assert!(output.contains("downloads=\"100000000\""));
        assert!(output.contains("</crate>"));
    }

    #[test]
    fn format_details() {
        let details = CrateDetails {
            name: "serde".to_string(),
            description: Some("A serialization framework".to_string()),
            homepage: Some("https://serde.rs".to_string()),
            repository: Some("https://github.com/serde-rs/serde".to_string()),
            documentation: Some("https://docs.rs/serde".to_string()),
            license: Some("MIT/Apache-2.0".to_string()),
            keywords: vec!["serialization".to_string()],
            downloads: 100_000_000,
            max_version: "1.0.0".to_string(),
        };
        let output = format_crate_details(&details);
        assert!(output.contains("<crate name=\"serde\" version=\"1.0.0\""));
        assert!(output.contains("license: MIT/Apache-2.0"));
    }

    #[test]
    fn format_versions() {
        let versions = vec![
            CrateVersion {
                num: "1.0.0".to_string(),
                yanked: false,
            },
            CrateVersion {
                num: "0.9.0".to_string(),
                yanked: true,
            },
        ];
        let output = format_crate_versions(&versions);
        assert!(output.contains("<version num=\"1.0.0\" status=\"available\""));
        assert!(output.contains("<version num=\"0.9.0\" status=\"yanked\""));
    }

    #[test]
    fn format_deps() {
        let deps = vec![
            CrateDependency {
                crate_id: "proc-macro2".to_string(),
                req: "^1.0".to_string(),
                kind: "normal".to_string(),
                optional: false,
                default_features: true,
                features: vec![],
            },
            CrateDependency {
                crate_id: "quote".to_string(),
                req: "^1.0".to_string(),
                kind: "dev".to_string(),
                optional: true,
                default_features: true,
                features: vec!["proc-macro".to_string()],
            },
        ];
        let output = format_crate_deps(&deps);
        assert!(output.contains("<dependency name=\"proc-macro2\""));
        assert!(output.contains("kind=\"normal\""));
        assert!(output.contains("<dependency name=\"quote\""));
        assert!(output.contains("kind=\"dev\""));
        assert!(output.contains("optional"));
    }
}
