//! A one-shot HTTP listener on 127.0.0.1 that receives the OAuth redirect.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use super::AuthError;

const SUCCESS_PAGE: &str = "<!doctype html><html><body style=\"font-family:sans-serif\"><h2>Rail is connected.</h2><p>You can close this tab and return to the terminal.</p></body></html>";

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
) -> Option<Result<String, AuthError>> {
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
    let (status, body) = match &outcome {
        Some(Ok(_)) => ("200 OK", SUCCESS_PAGE.to_string()),
        Some(Err(e)) => (
            "400 Bad Request",
            format!(
                "<html><body><h2>Rail: login failed</h2><p>{}</p></body></html>",
                html_escape(&e.to_string())
            ),
        ),
        None => ("404 Not Found", String::new()),
    };
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

/// `None` for requests that are not the OAuth redirect (favicon, probes).
pub fn parse_redirect(path: &str, expected_state: &str) -> Option<Result<String, AuthError>> {
    let url = url::Url::parse(&format!("http://127.0.0.1{path}")).ok()?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => description = Some(value.into_owned()),
            _ => {}
        }
    }
    if code.is_none() && error.is_none() && state.is_none() {
        return None;
    }
    // Without the right state this is not our redirect: ignore it so a stray or
    // hostile request cannot abort a login in progress.
    if state.as_deref() != Some(expected_state) {
        tracing::debug!("ignoring loopback redirect with a missing or mismatched state");
        return None;
    }
    if let Some(error) = error {
        return Some(Err(AuthError::Denied(description.unwrap_or(error))));
    }
    Some(code.ok_or(AuthError::MissingCode))
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
        assert_eq!(r, "abc def");
    }

    #[test]
    fn parse_ignores_state_mismatch() {
        assert!(parse_redirect("/?code=abc&state=other", "s1").is_none());
        assert!(parse_redirect("/?code=abc", "s1").is_none());
        assert!(parse_redirect("/?error=access_denied", "s1").is_none());
        assert!(parse_redirect("/?error=access_denied&state=other", "s1").is_none());
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
}
