use reqwest::Client;
use serde::Deserialize;

use crate::error::{ToolError, ToolResult};

#[derive(Clone, Debug, Deserialize)]
pub struct CratesIoResponse {
    /// The list of crates returned by the search.
    crates: Vec<Crate>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Crate {
    /// The crates.io crate ID.
    pub id: String,
    /// The crate name.
    pub name: String,
    /// A brief description of the crate.
    pub description: Option<String>,
    /// URL to the crate's documentation.
    pub documentation: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CratesIoClient {
    /// The HTTP client used for API requests.
    http_client: Client,
}

impl CratesIoClient {
    /// Create a new `CratesIoClient`.
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
        }
    }

    /// Search crates.io for crates matching the given query.
    ///
    /// # Errors
    ///
    /// Returns `ToolError::Http` if the request fails at the transport level,
    /// or `ToolError::ApiError` if the API returns a non-success status.
    pub async fn search_crates_io(&self, query: &str) -> ToolResult<Vec<Crate>> {
        let endpoint = search_url(query);
        let results = self.get_from_crates_io(&endpoint).await?;

        Ok(results.crates)
    }

    /// Fetch a response from the crates.io API.
    ///
    /// # Errors
    ///
    /// Returns `ToolError::Http` on transport errors or `ToolError::ApiError`
    /// for non-success HTTP status codes.
    async fn get_from_crates_io(&self, endpoint: &str) -> ToolResult<CratesIoResponse> {
        let response = self
            .http_client
            .get(endpoint)
            .send()
            .await
            .map_err(|e| ToolError::Http { source: e })?;

        // Check for non-success status before attempting to deserialize.
        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(ToolError::ApiError {
                status: status.as_u16(),
                message: text,
            });
        }

        let body: CratesIoResponse = response
            .json()
            .await
            .map_err(|e| ToolError::Http { source: e })?;

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

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn search_url_is_correct() {
        assert_eq!(
            search_url("serde"),
            "https://crates.io/api/v1/crates?q=serde"
        );
    }
}
