//! A one-shot HTTP listener on 127.0.0.1 that receives the OAuth redirect.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use super::AuthError;

const SUCCESS_PAGE: &str = "<!doctype html><html><body style=\"font-family:sans-serif\"><h2>Rail is connected.</h2><p>You can close this tab and return to the terminal.</p></body></html>";

const CONSENT_PAGE: &str = "<!doctype html><html><body style=\"font-family:sans-serif\"><h2>Rail is approved for your organisation.</h2><p>You can close this tab and return to the terminal.</p></body></html>";

/// What a loopback redirect carried.
#[derive(Clone, PartialEq, Eq)]
pub enum Redirect {
    /// An authorization code, to exchange for tokens.
    Code(String),
    /// Microsoft's admin-consent confirmation for `tenant`.
    AdminConsent { tenant: String },
}

/// Hides the authorization code, which must never reach a log or `Debug` output.
impl std::fmt::Debug for Redirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(_) => f.write_str("Code(<redacted>)"),
            Self::AdminConsent { tenant } => f
                .debug_struct("AdminConsent")
                .field("tenant", tenant)
                .finish(),
        }
    }
}

/// Longest a connection may take to send its request before it is dropped.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Listener bound to an ephemeral loopback port.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
    request_timeout: Duration,
}

impl Loopback {
    /// Bind 127.0.0.1 on a free port.
    pub async fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            port,
            request_timeout: REQUEST_READ_TIMEOUT,
        })
    }

    /// Override the per-connection read timeout (tests only).
    #[cfg(test)]
    pub(crate) fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// The redirect URI to register with the authorization request.
    pub fn redirect_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Serve requests until one carries the OAuth redirect, then return its code.
    /// Only a failure to accept ends the flow early; stray connections are dropped.
    pub async fn wait_for_code(self, expected_state: &str) -> Result<String, AuthError> {
        match self.wait_for_redirect(expected_state).await? {
            Redirect::Code(code) => Ok(code),
            Redirect::AdminConsent { .. } => Err(AuthError::MissingCode),
        }
    }

    /// Serve requests until the admin-consent redirect arrives; return the consenting tenant.
    pub async fn wait_for_admin_consent(self, expected_state: &str) -> Result<String, AuthError> {
        match self.wait_for_redirect(expected_state).await? {
            Redirect::AdminConsent { tenant } => Ok(tenant),
            Redirect::Code(_) => Err(AuthError::MissingConsent),
        }
    }

    async fn wait_for_redirect(self, expected_state: &str) -> Result<Redirect, AuthError> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            if let Some(result) =
                handle_connection(stream, expected_state, self.request_timeout).await
            {
                return result;
            }
        }
    }
}

/// Answer one connection. `None` means it was not the redirect (or broke); keep listening.
async fn handle_connection(
    stream: TcpStream,
    expected_state: &str,
    timeout: Duration,
) -> Option<Result<Redirect, AuthError>> {
    let mut reader = BufReader::new(stream);
    let request_line = match tokio::time::timeout(timeout, read_request(&mut reader)).await {
        Ok(Ok(line)) => line,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "dropping loopback connection after read error");
            return None;
        }
        Err(_) => {
            tracing::debug!("dropping idle loopback connection after read timeout");
            return None;
        }
    };
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let outcome = parse_redirect(path, expected_state);
    let (status, body) = response_for(&outcome);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = reader.into_inner();
    if let Err(e) = stream.write_all(response.as_bytes()).await {
        tracing::debug!(error = %e, "could not answer loopback connection");
    }
    let _ = stream.shutdown().await;
    outcome
}

fn response_for(outcome: &Option<Result<Redirect, AuthError>>) -> (&'static str, String) {
    match outcome {
        Some(Ok(Redirect::Code(_))) => ("200 OK", SUCCESS_PAGE.to_string()),
        Some(Ok(Redirect::AdminConsent { .. })) => ("200 OK", CONSENT_PAGE.to_string()),
        Some(Err(e)) => (
            "400 Bad Request",
            format!(
                "<html><body><h2>Rail: login failed</h2><p>{}</p></body></html>",
                html_escape(&e.to_string())
            ),
        ),
        None => ("404 Not Found", String::new()),
    }
}

/// Read the request line and discard the headers.
async fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<String> {
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    drain_headers(reader).await?;
    Ok(request_line)
}

/// Escape text for safe interpolation into HTML (the error text can be provider-controlled).
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Read and discard header lines up to the blank line. Closing a socket with unread
/// bytes sends a TCP reset, which makes browsers show an error instead of our page.
async fn drain_headers(reader: &mut BufReader<TcpStream>) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line == "\r\n" || line == "\n" {
            return Ok(());
        }
    }
}

/// `None` for requests that are not the OAuth redirect (favicon, probes) and for redirects
/// whose `state` is absent or wrong, so a stray or hostile request cannot end a login or a
/// consent in progress.
pub fn parse_redirect(path: &str, expected_state: &str) -> Option<Result<Redirect, AuthError>> {
    let url = url::Url::parse(&format!("http://127.0.0.1{path}")).ok()?;
    let query = RedirectQuery::from_url(&url);
    if query.is_empty() {
        return None;
    }
    if query.state.as_deref() != Some(expected_state) {
        tracing::debug!("ignoring loopback redirect with a missing or mismatched state");
        return None;
    }
    Some(query.into_outcome())
}

#[derive(Default)]
struct RedirectQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    description: Option<String>,
    admin_consent: Option<String>,
    tenant: Option<String>,
}

impl RedirectQuery {
    fn from_url(url: &url::Url) -> Self {
        let mut query = Self::default();
        for (key, value) in url.query_pairs() {
            let slot = match key.as_ref() {
                "code" => &mut query.code,
                "state" => &mut query.state,
                "error" => &mut query.error,
                "error_description" => &mut query.description,
                "admin_consent" => &mut query.admin_consent,
                "tenant" => &mut query.tenant,
                _ => continue,
            };
            *slot = Some(value.into_owned());
        }
        query
    }

    fn is_empty(&self) -> bool {
        self.code.is_none()
            && self.error.is_none()
            && self.state.is_none()
            && self.admin_consent.is_none()
    }

    /// The provider's error wins, then a consent confirmation, then the code.
    fn into_outcome(self) -> Result<Redirect, AuthError> {
        if let Some(error) = self.error {
            return Err(AuthError::Denied(self.description.unwrap_or(error)));
        }
        let consented = self
            .admin_consent
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        if consented {
            return Ok(Redirect::AdminConsent {
                tenant: self.tenant.unwrap_or_default(),
            });
        }
        self.code.map(Redirect::Code).ok_or(AuthError::MissingCode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ignores_unrelated_requests() {
        assert!(parse_redirect("/favicon.ico", "s").is_none());
        assert!(parse_redirect("/", "s").is_none());
    }

    #[test]
    fn parse_returns_code_when_state_matches() {
        let r = parse_redirect("/?code=abc%20def&state=s1", "s1")
            .unwrap()
            .unwrap();
        assert_eq!(r, Redirect::Code("abc def".into()));
    }

    #[test]
    fn parse_returns_admin_consent_when_state_matches() {
        let r = parse_redirect("/?admin_consent=True&tenant=tid-1&state=s1", "s1")
            .unwrap()
            .unwrap();
        assert_eq!(
            r,
            Redirect::AdminConsent {
                tenant: "tid-1".into()
            }
        );
    }

    #[test]
    fn parse_ignores_admin_consent_with_a_mismatched_or_absent_state() {
        assert!(parse_redirect("/?admin_consent=True&tenant=t&state=other", "s1").is_none());
        assert!(parse_redirect("/?admin_consent=True&tenant=t", "s1").is_none());
    }

    #[test]
    fn parse_reports_a_consent_error_with_its_description() {
        let r = parse_redirect(
            "/?error=access_denied&error_description=AADSTS65004%3A%20declined&state=s1",
            "s1",
        )
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(r, AuthError::Denied(ref m) if m == "AADSTS65004: declined"),
            "{r:?}"
        );
    }

    #[test]
    fn redirect_debug_hides_the_code() {
        let debug = format!("{:?}", Redirect::Code("secret-code".into()));
        assert!(!debug.contains("secret-code"), "{debug}");
    }

    #[test]
    fn parse_ignores_state_mismatch() {
        assert!(parse_redirect("/?code=abc&state=other", "s1").is_none());
        assert!(parse_redirect("/?code=abc", "s1").is_none());
        assert!(parse_redirect("/?error=access_denied", "s1").is_none());
        assert!(parse_redirect("/?error=access_denied&state=other", "s1").is_none());
        assert!(parse_redirect("/?admin_consent=True&tenant=t&state=other", "s1").is_none());
        assert!(parse_redirect("/?admin_consent=True&tenant=t", "s1").is_none());
    }

    #[test]
    fn parse_reports_missing_code_when_state_matches() {
        let r = parse_redirect("/?state=s1", "s1").unwrap().unwrap_err();
        assert!(matches!(r, AuthError::MissingCode));
    }

    #[test]
    fn parse_reports_provider_error_with_description() {
        let r = parse_redirect(
            "/?error=access_denied&error_description=User%20cancelled&state=s1",
            "s1",
        )
        .unwrap()
        .unwrap_err();
        match r {
            AuthError::Denied(msg) => assert_eq!(msg, "User cancelled"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn html_escape_escapes_markup_characters() {
        assert_eq!(
            html_escape(r#"<script>alert("x")&'y'</script>"#),
            "&lt;script&gt;alert(&quot;x&quot;)&amp;&#39;y&#39;&lt;/script&gt;"
        );
    }

    #[tokio::test]
    async fn listener_answers_favicon_then_returns_code() {
        let lb = Loopback::bind().await.unwrap();
        let base = lb.redirect_url();
        let browser = tokio::spawn(async move {
            let favicon = reqwest::get(format!("{base}/favicon.ico")).await.unwrap();
            assert_eq!(favicon.status(), 404);
            let done = reqwest::get(format!("{base}/?code=c0de&state=st"))
                .await
                .unwrap();
            assert_eq!(done.status(), 200);
            assert!(done.text().await.unwrap().contains("close this tab"));
        });
        let code = lb.wait_for_code("st").await.unwrap();
        assert_eq!(code, "c0de");
        browser.await.unwrap();
    }

    #[tokio::test]
    async fn idle_connection_does_not_block_the_redirect() {
        let lb = Loopback::bind()
            .await
            .unwrap()
            .with_request_timeout(Duration::from_millis(100));
        let addr = lb.listener.local_addr().unwrap();
        let _idle = TcpStream::connect(addr).await.unwrap();
        let base = lb.redirect_url();
        let browser = tokio::spawn(async move {
            reqwest::get(format!("{base}/?code=c0de&state=st"))
                .await
                .unwrap();
        });
        assert_eq!(lb.wait_for_code("st").await.unwrap(), "c0de");
        browser.await.unwrap();
    }

    #[tokio::test]
    async fn garbage_request_does_not_abort_the_listener() {
        let lb = Loopback::bind().await.unwrap();
        let addr = lb.listener.local_addr().unwrap();
        let base = lb.redirect_url();
        let browser = tokio::spawn(async move {
            let mut junk = TcpStream::connect(addr).await.unwrap();
            junk.write_all(&[0xff, 0xfe, 0xfd, b'\n']).await.unwrap();
            drop(junk);
            reqwest::get(format!("{base}/?code=c0de&state=st"))
                .await
                .unwrap();
        });
        assert_eq!(lb.wait_for_code("st").await.unwrap(), "c0de");
        browser.await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_admin_consent_ignores_stray_requests() {
        let lb = Loopback::bind().await.unwrap();
        let base = lb.redirect_url();
        let browser = tokio::spawn(async move {
            let favicon = reqwest::get(format!("{base}/favicon.ico")).await.unwrap();
            assert_eq!(favicon.status(), 404);
            let forged = reqwest::get(format!("{base}/?admin_consent=True&tenant=evil&state=nope"))
                .await
                .unwrap();
            assert_eq!(forged.status(), 404);
            let done = reqwest::get(format!("{base}/?admin_consent=True&tenant=tid&state=st"))
                .await
                .unwrap();
            assert_eq!(done.status(), 200);
            assert!(done.text().await.unwrap().contains("approved"));
        });
        assert_eq!(lb.wait_for_admin_consent("st").await.unwrap(), "tid");
        browser.await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_code_rejects_a_consent_redirect() {
        let lb = Loopback::bind().await.unwrap();
        let base = lb.redirect_url();
        tokio::spawn(async move {
            reqwest::get(format!("{base}/?admin_consent=True&tenant=t&state=st"))
                .await
                .unwrap();
        });
        let err = lb.wait_for_code("st").await.unwrap_err();
        assert!(matches!(err, AuthError::MissingCode), "{err:?}");
    }
}
