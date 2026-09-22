use crate::application::DecisionError;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// The TypeSafe judgment API. Every operation is one `systemone` call.
const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
/// A judgment answer is small; anything larger is a wrong endpoint.
const RESPONSE_LIMIT: usize = 1 << 20;
const TIMEOUT_SECONDS: u32 = 30;
/// `curl`'s exit code for an operation that ran out of time.
const CURL_TIMEOUT_EXIT: i32 = 28;

/// Talks to the TypeSafe `/v1/systemone` endpoint.
///
/// `curl` is spawned with process args rather than a shell, exactly as the
/// update checker does; July still adds no HTTP client of its own. The API key
/// travels in a config document on `curl`'s stdin, so it appears neither in
/// `argv` - where any local process could read it - nor on disk.
#[derive(Clone, Debug)]
pub struct JevClient {
    base_url: String,
    api_key: String,
}

impl JevClient {
    /// `None` when no API key is configured; the engine turns that into
    /// `DecisionError::ProviderNotConfigured` rather than guessing an answer.
    pub fn from_env() -> Option<Self> {
        let api_key = env_value("TYPESAFE_API_KEY")?;
        Some(Self {
            base_url: env_value("JULY_JEV_BASE_URL")
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned())
                .trim_end_matches('/')
                .to_owned(),
            api_key,
        })
    }

    pub async fn systemone(&self, body: &Value) -> Result<Value, DecisionError> {
        let config = curl_config(
            &format!("{}/v1/systemone", self.base_url),
            &self.api_key,
            &body.to_string(),
        );
        let mut child = tokio::process::Command::new("curl")
            .args(["--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                DecisionError::Unavailable(format!("curl could not start: {error}"))
            })?;
        let mut stdin = child.stdin.take().expect("curl stdin was piped");
        stdin
            .write_all(config.as_bytes())
            .await
            .map_err(|error| DecisionError::Unavailable(format!("curl refused input: {error}")))?;
        drop(stdin);

        let output = child
            .wait_with_output()
            .await
            .map_err(|error| DecisionError::Unavailable(format!("curl did not finish: {error}")))?;
        if !output.status.success() {
            if output.status.code() == Some(CURL_TIMEOUT_EXIT) {
                return Err(DecisionError::Timeout);
            }
            let reason = String::from_utf8_lossy(&output.stderr);
            let reason = reason.trim();
            return Err(DecisionError::Unavailable(if reason.is_empty() {
                format!("curl exited with {}", output.status)
            } else {
                reason.to_owned()
            }));
        }
        let response = String::from_utf8(output.stdout)
            .map_err(|_| DecisionError::InvalidResponse("response is not UTF-8".to_owned()))?;
        parse_response(&response)
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Build the `curl` config document. Only `\` and `"` need escaping inside a
/// quoted config parameter, and a serialized JSON body contains no raw control
/// characters, so the body round-trips byte for byte.
fn curl_config(url: &str, api_key: &str, body: &str) -> String {
    format!(
        "url = \"{url}\"\n\
         request = \"POST\"\n\
         silent\n\
         show-error\n\
         proto = \"=https\"\n\
         tlsv1.2\n\
         max-time = {TIMEOUT_SECONDS}\n\
         max-filesize = {RESPONSE_LIMIT}\n\
         user-agent = \"july/{version}\"\n\
         header = \"Content-Type: application/json\"\n\
         header = \"Authorization: Bearer {key}\"\n\
         write-out = \"\\n%{{http_code}}\"\n\
         data = \"{body}\"\n",
        url = quote(url),
        version = env!("CARGO_PKG_VERSION"),
        key = quote(api_key),
        body = quote(body),
    )
}

fn quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Split the trailing status code `--write-out` appended, then judge it.
fn parse_response(response: &str) -> Result<Value, DecisionError> {
    let (body, code) = response
        .rsplit_once('\n')
        .ok_or_else(|| DecisionError::InvalidResponse("response carried no status".to_owned()))?;
    let code: u16 = code
        .trim()
        .parse()
        .map_err(|_| DecisionError::InvalidResponse(format!("unreadable status {code:?}")))?;
    match code {
        200 => serde_json::from_str(body)
            .map_err(|error| DecisionError::InvalidResponse(format!("malformed JSON: {error}"))),
        401 | 403 => Err(DecisionError::Unavailable(
            "TypeSafe rejected the API key".to_owned(),
        )),
        408 | 504 => Err(DecisionError::Timeout),
        429 => Err(DecisionError::Unavailable("rate limited".to_owned())),
        code => Err(DecisionError::Unavailable(format!("HTTP {code}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_api_key_and_body_travel_in_the_config_document_not_in_argv() {
        let config = curl_config(
            "https://api.typesafe.ai/v1/systemone",
            "apikey_secret",
            &json!({ "task": "fix \"redis\" timeout" }).to_string(),
        );

        assert!(config.contains("header = \"Authorization: Bearer apikey_secret\"\n"));
        assert!(config.contains("proto = \"=https\"\n"));
        assert!(config.contains("url = \"https://api.typesafe.ai/v1/systemone\"\n"));
        // Every quote inside the JSON body is escaped for the config parser.
        assert!(
            config
                .contains("data = \"{\\\"task\\\":\\\"fix \\\\\\\"redis\\\\\\\" timeout\\\"}\"\n")
        );
    }

    #[test]
    fn a_successful_response_parses_and_the_status_line_is_dropped() {
        let body = parse_response("{\"answers\":{\"which\":{\"choice\":\"infra\"}}}\n200").unwrap();
        assert_eq!(body["answers"]["which"]["choice"], json!("infra"));
    }

    #[test]
    fn transport_and_provider_failures_map_to_distinct_decision_errors() {
        assert_eq!(
            parse_response("\n401").unwrap_err(),
            DecisionError::Unavailable("TypeSafe rejected the API key".to_owned())
        );
        assert_eq!(
            parse_response("\n429").unwrap_err(),
            DecisionError::Unavailable("rate limited".to_owned())
        );
        assert_eq!(parse_response("\n504").unwrap_err(), DecisionError::Timeout);
        assert_eq!(
            parse_response("\n500").unwrap_err(),
            DecisionError::Unavailable("HTTP 500".to_owned())
        );
        assert!(matches!(
            parse_response("not json\n200").unwrap_err(),
            DecisionError::InvalidResponse(_)
        ));
        assert!(matches!(
            parse_response("no status line").unwrap_err(),
            DecisionError::InvalidResponse(_)
        ));
    }
}
