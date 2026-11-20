// SPDX-License-Identifier: GPL-3.0-or-later

//! Thin client for Sunshine's local web API plus the pure JSON shapes involved.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Generated credentials for one Sunshine instance.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// One pending pairing request advertised by Sunshine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinRequest {
    pub id: String,
    pub name: String,
    pub address: String,
}

/// One paired client certificate known to Sunshine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEntry {
    pub name: String,
    pub uuid: String,
    pub enabled: bool,
}

/// Non-success HTTP response from Sunshine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub body: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sunshine API error {}: {}", self.status, self.body)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiFailure {
    #[error("{0}")]
    Http(ApiError),
    #[error("Sunshine credentials are no longer valid; reset the instance to pair again")]
    CredentialsLost,
    #[error("Sunshine has no administrator credentials yet")]
    WelcomeRequired,
    #[error("cannot reach the Sunshine API: {0}")]
    Transport(String),
    #[error("invalid Sunshine API response: {0}")]
    Decode(String),
}

fn transport(error: reqwest::Error) -> ApiFailure {
    ApiFailure::Transport(error.to_string())
}

fn decode(error: serde_json::Error) -> ApiFailure {
    ApiFailure::Decode(error.to_string())
}

fn string_field(value: &Value, field: &str) -> Result<String, ApiFailure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ApiFailure::Decode(format!("missing or invalid string field '{field}'")))
}

/// Request body for `POST /api/password`.
pub fn password_body(username: &str, password: &str) -> Value {
    json!({
        "currentUsername": "",
        "currentPassword": "",
        "newUsername": username,
        "newPassword": password,
        "confirmNewPassword": password,
    })
}

/// Request body for `POST /api/pin`.
pub fn pin_body(pairing_id: &str, pin: &str, name: &str) -> Value {
    json!({
        "pairing_id": pairing_id,
        "pin": pin,
        "name": name,
    })
}

/// Request body for `DELETE /api/pin`.
pub fn cancel_pin_body(pairing_id: &str) -> Value {
    json!({ "pairing_id": pairing_id })
}

/// Request body for `POST /api/clients/unpair`.
pub fn unpair_body(uuid: &str) -> Value {
    json!({ "uuid": uuid })
}

/// Request body for `POST /api/clients/update`.
pub fn client_enabled_body(uuid: &str, enabled: bool) -> Value {
    json!({ "uuid": uuid, "enabled": enabled })
}

/// Parse a `GET /api/pin` response.
pub fn parse_pin_response(value: &Value) -> Result<Vec<PinRequest>, ApiFailure> {
    let entries = value
        .get("pairings")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiFailure::Decode("missing 'pairings' array".to_string()))?;
    entries
        .iter()
        .map(|entry| {
            Ok(PinRequest {
                id: string_field(entry, "id")?,
                name: string_field(entry, "name")?,
                address: string_field(entry, "address")?,
            })
        })
        .collect()
}

/// Parse a `GET /api/clients/list` response.
pub fn parse_clients_response(value: &Value) -> Result<Vec<ClientEntry>, ApiFailure> {
    if let Some(status) = value.get("status")
        && status.as_bool() != Some(true)
    {
        return Err(ApiFailure::Decode(
            "Sunshine reported status=false for the client list".to_string(),
        ));
    }
    let entries = value
        .get("named_certs")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiFailure::Decode("missing 'named_certs' array".to_string()))?;
    entries
        .iter()
        .map(|entry| {
            let enabled = entry
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    ApiFailure::Decode("missing or invalid boolean field 'enabled'".to_string())
                })?;
            Ok(ClientEntry {
                name: string_field(entry, "name")?,
                uuid: string_field(entry, "uuid")?,
                enabled,
            })
        })
        .collect()
}

/// Sunshine web API client bound to one instance's loopback port.
pub struct SunshineApi {
    client: reqwest::Client,
    base: String,
    creds: Option<Credentials>,
}

impl fmt::Debug for SunshineApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SunshineApi")
            .field("base", &self.base)
            .field("has_credentials", &self.creds.is_some())
            .finish()
    }
}

impl SunshineApi {
    pub fn new(web_port: u16, creds: Option<Credentials>) -> Self {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build the Sunshine HTTP client");
        Self {
            client,
            base: format!("https://127.0.0.1:{web_port}"),
            creds,
        }
    }

    pub fn with_credentials(mut self, creds: Credentials) -> Self {
        self.creds = Some(creds);
        self
    }

    /// Base URL without a trailing slash, for diagnostics and tests.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.creds {
            Some(creds) => request.basic_auth(&creds.username, Some(&creds.password)),
            None => request,
        }
    }

    async fn finish(
        &self,
        response: reqwest::Response,
        authenticated: bool,
    ) -> Result<Value, ApiFailure> {
        let status = response.status().as_u16();
        let body = response.text().await.map_err(transport)?;
        if status == 401 && authenticated && self.creds.is_some() {
            return Err(ApiFailure::CredentialsLost);
        }
        if !(200..300).contains(&status) {
            return Err(ApiFailure::Http(ApiError { status, body }));
        }
        serde_json::from_str(&body).map_err(decode)
    }

    async fn finish_empty(
        &self,
        response: reqwest::Response,
        authenticated: bool,
    ) -> Result<(), ApiFailure> {
        let status = response.status().as_u16();
        let body = response.text().await.map_err(transport)?;
        if status == 401 && authenticated && self.creds.is_some() {
            return Err(ApiFailure::CredentialsLost);
        }
        if !(200..300).contains(&status) {
            return Err(ApiFailure::Http(ApiError { status, body }));
        }
        Ok(())
    }

    /// `GET /api/configLocale`: readiness probe, no authentication.
    pub async fn config_locale(&self) -> Result<Value, ApiFailure> {
        let response = self
            .client
            .get(self.url("/api/configLocale"))
            .send()
            .await
            .map_err(transport)?;
        self.finish(response, false).await
    }

    /// `GET /api/config`: verifies the credentials; 401 means they are lost.
    pub async fn config(&self) -> Result<Value, ApiFailure> {
        let response = self
            .auth(self.client.get(self.url("/api/config")))
            .send()
            .await
            .map_err(transport)?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(ApiFailure::CredentialsLost);
        }
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if location.contains("welcome") {
                return Err(ApiFailure::WelcomeRequired);
            }
            return Err(ApiFailure::Http(ApiError {
                status,
                body: format!("unexpected redirect to {location}"),
            }));
        }
        self.finish(response, self.creds.is_some()).await
    }

    /// `POST /api/password`: first-time credential setup.
    pub async fn set_password(&self, username: &str, password: &str) -> Result<(), ApiFailure> {
        let response = self
            .client
            .post(self.url("/api/password"))
            .json(&password_body(username, password))
            .send()
            .await
            .map_err(transport)?;
        self.finish_empty(response, false).await
    }

    /// `GET /api/pin`: pending pairing requests.
    pub async fn pin_pairings(&self) -> Result<Vec<PinRequest>, ApiFailure> {
        let response = self
            .auth(self.client.get(self.url("/api/pin")))
            .send()
            .await
            .map_err(transport)?;
        let value = self.finish(response, true).await?;
        parse_pin_response(&value)
    }

    /// `POST /api/pin`: submit a PIN for a pending request.
    pub async fn submit_pin(
        &self,
        pairing_id: &str,
        pin: &str,
        name: &str,
    ) -> Result<bool, ApiFailure> {
        let response = self
            .auth(self.client.post(self.url("/api/pin")))
            .json(&pin_body(pairing_id, pin, name))
            .send()
            .await
            .map_err(transport)?;
        let value = self.finish(response, true).await?;
        value
            .get("status")
            .and_then(Value::as_bool)
            .ok_or_else(|| ApiFailure::Decode("missing or invalid 'status' boolean".to_string()))
    }

    /// `DELETE /api/pin`: reject a pending request.
    pub async fn cancel_pin(&self, pairing_id: &str) -> Result<(), ApiFailure> {
        let response = self
            .auth(self.client.delete(self.url("/api/pin")))
            .json(&cancel_pin_body(pairing_id))
            .send()
            .await
            .map_err(transport)?;
        self.finish_empty(response, true).await
    }

    /// `GET /api/clients/list`: paired client certificates.
    pub async fn clients(&self) -> Result<Vec<ClientEntry>, ApiFailure> {
        let response = self
            .auth(self.client.get(self.url("/api/clients/list")))
            .send()
            .await
            .map_err(transport)?;
        let value = self.finish(response, true).await?;
        parse_clients_response(&value)
    }

    /// `POST /api/clients/unpair`.
    pub async fn unpair(&self, uuid: &str) -> Result<(), ApiFailure> {
        let response = self
            .auth(self.client.post(self.url("/api/clients/unpair")))
            .json(&unpair_body(uuid))
            .send()
            .await
            .map_err(transport)?;
        self.finish_empty(response, true).await
    }

    /// `POST /api/clients/update`.
    pub async fn set_client_enabled(&self, uuid: &str, enabled: bool) -> Result<(), ApiFailure> {
        let response = self
            .auth(self.client.post(self.url("/api/clients/update")))
            .json(&client_enabled_body(uuid, enabled))
            .send()
            .await
            .map_err(transport)?;
        self.finish_empty(response, true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_uses_loopback_and_port() {
        assert_eq!(
            SunshineApi::new(48050, None).base_url(),
            "https://127.0.0.1:48050"
        );
        let creds = Credentials {
            username: "longcar".into(),
            password: "secret".into(),
        };
        let api = SunshineApi::new(48110, Some(creds)).with_credentials(Credentials {
            username: "longcar".into(),
            password: "next".into(),
        });
        assert_eq!(api.base_url(), "https://127.0.0.1:48110");
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let creds = Credentials {
            username: "longcar".into(),
            password: "hunter2".into(),
        };
        let text = format!("{creds:?}");
        assert!(text.contains("longcar"));
        assert!(!text.contains("hunter2"));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn password_body_shape() {
        assert_eq!(
            password_body("longcar", "abc123"),
            json!({
                "currentUsername": "",
                "currentPassword": "",
                "newUsername": "longcar",
                "newPassword": "abc123",
                "confirmNewPassword": "abc123",
            })
        );
    }

    #[test]
    fn pin_and_client_bodies() {
        assert_eq!(
            pin_body("00112233445566778899aabbccddeeff", "1234", "Tablet"),
            json!({
                "pairing_id": "00112233445566778899aabbccddeeff",
                "pin": "1234",
                "name": "Tablet",
            })
        );
        assert_eq!(cancel_pin_body("abc"), json!({ "pairing_id": "abc" }));
        assert_eq!(unpair_body("uuid-1"), json!({ "uuid": "uuid-1" }));
        assert_eq!(
            client_enabled_body("uuid-1", false),
            json!({ "uuid": "uuid-1", "enabled": false })
        );
    }

    #[test]
    fn parses_pin_response() {
        let value = json!({
            "pairings": [
                {"id": "aa", "name": "Tablet", "address": "192.168.2.20"},
                {"id": "bb", "name": "Phone", "address": "192.168.2.21"},
            ]
        });
        assert_eq!(
            parse_pin_response(&value).unwrap(),
            vec![
                PinRequest {
                    id: "aa".into(),
                    name: "Tablet".into(),
                    address: "192.168.2.20".into(),
                },
                PinRequest {
                    id: "bb".into(),
                    name: "Phone".into(),
                    address: "192.168.2.21".into(),
                },
            ]
        );
        assert_eq!(
            parse_pin_response(&json!({ "pairings": [] })).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn rejects_malformed_pin_response() {
        for value in [
            json!({}),
            json!({"pairings": [
                {"id": "aa", "name": "Tablet"}
            ]}),
            json!({"pairings": [
                {"id": 7, "name": "Tablet", "address": "x"}
            ]}),
            json!({"pairings": "nope"}),
        ] {
            assert!(matches!(
                parse_pin_response(&value),
                Err(ApiFailure::Decode(_))
            ));
        }
    }

    #[test]
    fn parses_clients_response() {
        let value = json!({
            "status": true,
            "named_certs": [
                {"name": "Tablet", "uuid": "u-1", "enabled": true},
                {"name": "Phone", "uuid": "u-2", "enabled": false},
            ]
        });
        assert_eq!(
            parse_clients_response(&value).unwrap(),
            vec![
                ClientEntry {
                    name: "Tablet".into(),
                    uuid: "u-1".into(),
                    enabled: true,
                },
                ClientEntry {
                    name: "Phone".into(),
                    uuid: "u-2".into(),
                    enabled: false,
                },
            ]
        );
    }

    #[test]
    fn rejects_failed_or_malformed_client_responses() {
        assert!(matches!(
            parse_clients_response(&json!({"status": false, "named_certs": []})),
            Err(ApiFailure::Decode(_))
        ));
        for value in [
            json!({"status": true}),
            json!({"status": true, "named_certs": [{"name": "x", "uuid": "y"}]}),
            json!({"status": true, "named_certs": [{"uuid": "y", "enabled": true}]}),
        ] {
            assert!(matches!(
                parse_clients_response(&value),
                Err(ApiFailure::Decode(_))
            ));
        }
    }

    #[test]
    fn api_error_display_is_exact() {
        let failure = ApiFailure::Http(ApiError {
            status: 500,
            body: "boom".into(),
        });
        assert_eq!(failure.to_string(), "Sunshine API error 500: boom");
        assert_eq!(
            ApiFailure::CredentialsLost.to_string(),
            "Sunshine credentials are no longer valid; reset the instance to pair again"
        );
    }
}
