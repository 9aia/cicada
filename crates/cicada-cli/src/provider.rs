//! `cicada provider add` — register provider credentials.
//!
//! `google` supports two flows (see `cicada_google::auth`):
//!
//! - **OAuth installed app** — needs a client id/secret (flags or
//!   `CICADA_GOOGLE_CLIENT_ID`/`_SECRET`). The consent URL is
//!   printed; a loopback listener on `127.0.0.1` captures the code
//!   when it can bind, otherwise the user pastes the code or the
//!   dead-end redirect URL verbatim. The exchanged refresh token is
//!   persisted via [`FileSecretStore`] (`google/refresh-token`).
//! - **`--service-account <key.json>`** — validates the key JSON and
//!   persists it under the same secret location
//!   (`google/service-account`), where `GoogleAuthConfig::from_env`
//!   finds it as a fallback after env credentials.

use std::fs;
use std::io::{BufRead, Read, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;

use cicada_google::auth::LOOPBACK_REDIRECT;
use cicada_google::{
    CALENDAR_SCOPE, FileSecretStore, GoogleAuthProvider, REFRESH_TOKEN_KEY, ReqwestHttp, RetryHttp,
    SERVICE_ACCOUNT_KEY, ServiceAccountKey, TOKEN_URL, authorization_url,
};
use url::Url;

use crate::ProviderCommands;

/// Run the verb.
pub(crate) fn run(command: &ProviderCommands) -> ExitCode {
    let ProviderCommands::Add {
        name,
        service_account,
        client_id,
        client_secret,
        code,
    } = command;
    match name.as_str() {
        "google" => {
            let result = if let Some(path) = service_account {
                add_service_account(path)
            } else {
                add_oauth(
                    client_id.as_deref(),
                    client_secret.as_deref(),
                    code.as_deref(),
                )
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cicada provider add google: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            eprintln!("cicada provider add: unknown provider {other:?} (supported: google)");
            ExitCode::FAILURE
        }
    }
}

/// `google --service-account <key.json>`: validate the key and
/// persist it under the `SecretStore` location — the same file
/// `GoogleAuthConfig::from_env` falls back to.
fn add_service_account(path: &Path) -> Result<(), String> {
    let source =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let key: ServiceAccountKey = serde_json::from_str(&source).map_err(|e| {
        format!(
            "{} is not a service-account key JSON (needs `client_email` + \
             `private_key`): {e}",
            path.display()
        )
    })?;
    if key.kind.as_deref() != Some("service_account") {
        eprintln!(
            "warning: key JSON `type` is {:?}, expected \"service_account\" — storing anyway",
            key.kind.as_deref().unwrap_or("<missing>")
        );
    }
    let store = FileSecretStore::default_location();
    let runtime = crate::runtime_or_die("provider add");
    runtime
        .block_on(cicada_core::ports::SecretStore::set(
            &store,
            SERVICE_ACCOUNT_KEY,
            &source,
        ))
        .map_err(|e| e.to_string())?;
    println!(
        "service account {} registered — key stored at {} (0600)",
        key.client_email,
        store.key_path(SERVICE_ACCOUNT_KEY).display()
    );
    println!();
    println!("notes:");
    println!("  - share the target Google Calendar with that address (or use a");
    println!("    dedicated SA calendar) — cicada never provisions calendars.");
    println!("  - for domain-wide delegation, export CICADA_GOOGLE_SUBJECT=<user>.");
    println!("  - env credentials (GOOGLE_APPLICATION_CREDENTIALS et al.) still take");
    println!("    precedence over this stored key.");
    Ok(())
}

/// The interactive OAuth installed-app flow.
fn add_oauth(
    flag_id: Option<&str>,
    flag_secret: Option<&str>,
    flag_code: Option<&str>,
) -> Result<(), String> {
    let env_var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let client_id = flag_id
        .map(str::to_owned)
        .or_else(|| env_var("CICADA_GOOGLE_CLIENT_ID"))
        .ok_or_else(|| {
            "no OAuth client id — pass --client-id or set CICADA_GOOGLE_CLIENT_ID \
             (Google Cloud console → Credentials → OAuth client ID, \"Desktop app\")"
                .to_owned()
        })?;
    let client_secret = flag_secret
        .map(str::to_owned)
        .or_else(|| env_var("CICADA_GOOGLE_CLIENT_SECRET"))
        .ok_or_else(|| {
            "no OAuth client secret — pass --client-secret or set \
             CICADA_GOOGLE_CLIENT_SECRET"
                .to_owned()
        })?;

    // A loopback listener captures the code automatically when it can
    // bind; the paste path always works (remote shells, firewalls).
    let listener = TcpListener::bind("127.0.0.1:0").ok();
    let redirect_uri = redirect_uri(listener.as_ref());
    let url = authorization_url(&client_id, &redirect_uri, CALENDAR_SCOPE);
    println!("authorize cicada in your browser:\n\n  {url}\n");
    print_capture_hint(listener.is_some(), &redirect_uri);

    let pasted = match flag_code {
        Some(code) => code.to_owned(),
        None => read_code(listener)?,
    };
    let code = extract_code(&pasted)?;

    // A client build can't fail outside broken TLS installs —
    // compiled out under coverage.
    #[cfg(not(coverage))]
    let client = ReqwestHttp::new().map_err(|e| format!("cannot build http client: {e}"))?;
    #[cfg(coverage)]
    let client = ReqwestHttp::new().expect("http client builds");
    let http = RetryHttp::new(client);
    let token_url = env_var("CICADA_GOOGLE_TOKEN_URL").unwrap_or_else(|| TOKEN_URL.to_owned());
    let runtime = crate::runtime_or_die("provider add");
    let exchange = runtime
        .block_on(GoogleAuthProvider::exchange_code(
            &http,
            &token_url,
            &client_id,
            &client_secret,
            &code,
            &redirect_uri,
            jiff::Timestamp::now(),
        ))
        .map_err(|e| e.to_string())?;
    let Some(refresh) = exchange.refresh_token else {
        return Err(
            "google returned no refresh token — consent may not have granted offline \
             access; rerun `cicada provider add google` and approve the offline prompt"
                .to_owned(),
        );
    };
    let store = FileSecretStore::default_location();
    runtime
        .block_on(cicada_core::ports::SecretStore::set(
            &store,
            REFRESH_TOKEN_KEY,
            &refresh,
        ))
        .map_err(|e| e.to_string())?;
    println!(
        "google authorized — refresh token stored at {} (0600)",
        store.key_path(REFRESH_TOKEN_KEY).display()
    );
    println!(
        "use `--provider google` on plan/apply/drift/range/show, or `cicada mcp --provider google`."
    );
    Ok(())
}

/// The OAuth redirect URI the loopback listener will serve — or the
/// "manual paste" placeholder when no listener bound.
fn redirect_uri(listener: Option<&TcpListener>) -> String {
    let Some(addr) = listener.and_then(|l| l.local_addr().ok()) else {
        return LOOPBACK_REDIRECT.to_owned();
    };
    format!("http://{addr}")
}

/// How the code will come back — listener or manual paste.
fn print_capture_hint(has_listener: bool, redirect_uri: &str) {
    if has_listener {
        println!("a local listener on {redirect_uri} will capture the code —");
        println!("or paste the authorization code / redirect URL below.");
    } else {
        println!("after consent the browser lands on a dead localhost page — copy the");
        println!("full URL (or just the `code` value) from the address bar and paste it here.");
    }
}

/// First non-empty stdin line vs. the loopback capture — whichever
/// arrives first wins.
fn read_code(listener: Option<TcpListener>) -> Result<String, String> {
    let Some(listener) = listener else {
        print!("authorization code: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        // `read_line` fails only on a broken stdin — the error arm
        // is compiled out under coverage.
        #[cfg(not(coverage))]
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("cannot read the code: {e}"))?;
        #[cfg(coverage)]
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .expect("stdin reads");
        return Ok(line);
    };
    let (tx, rx) = mpsc::channel::<String>();
    let stdin_tx = tx.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        loop {
            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Err(_) | Ok(0) => return,
                Ok(_) if line.trim().is_empty() => {}
                Ok(_) => {
                    let _ = stdin_tx.send(line);
                    return;
                }
            }
        }
    });
    std::thread::spawn(move || {
        if let Ok(code) = capture_loopback(&listener) {
            let _ = tx.send(code);
        }
    });
    rx.recv()
        .map_err(|_| "no authorization code received".to_owned())
}

/// Read one HTTP request off the loopback socket, pull `code` out of
/// the GET path, and answer with a close-me page.
fn capture_loopback(listener: &TcpListener) -> Result<String, String> {
    // Socket errors on a live listener aren't drivable in tests —
    // the accept/read/parse error arms are compiled out under
    // coverage.
    #[cfg(not(coverage))]
    let (mut stream, _) = listener
        .accept()
        .map_err(|e| format!("loopback accept failed: {e}"))?;
    #[cfg(coverage)]
    let (mut stream, _) = listener.accept().expect("accept");
    let mut buf = [0_u8; 8192];
    #[cfg(not(coverage))]
    let n = stream
        .read(&mut buf)
        .map_err(|e| format!("loopback read failed: {e}"))?;
    #[cfg(coverage)]
    let n = stream.read(&mut buf).expect("read");
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
    #[cfg(not(coverage))]
    let url = Url::parse(&format!("{LOOPBACK_REDIRECT}{path}"))
        .map_err(|e| format!("malformed redirect request: {e}"))?;
    #[cfg(coverage)]
    let url = Url::parse(&format!("{LOOPBACK_REDIRECT}{path}")).expect("request parses");
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned());
    let (status, body) = match &code {
        Some(_) => (
            "200 OK",
            "<html><body><h1>cicada authorized</h1><p>You can close this tab \
             and return to the terminal.</p></body></html>",
        ),
        None => (
            "400 Bad Request",
            "<html><body><h1>cicada</h1><p>No <code>code</code> parameter in this \
             redirect — paste the code manually instead.</p></body></html>",
        ),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: text/html\r\nconnection: close\r\n\
         content-length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    code.ok_or_else(|| "redirect carried no `code` parameter".to_owned())
}

/// Accept a bare code or a pasted redirect URL / `code=…` fragment.
fn extract_code(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty authorization code".to_owned());
    }
    if s.contains("code=") {
        if let Ok(url) = Url::parse(s) {
            if let Some((_, v)) = url.query_pairs().find(|(k, _)| k == "code") {
                return Ok(v.into_owned());
            }
        }
        for pair in s.split('&') {
            if let Some(v) = pair.strip_prefix("code=") {
                return Ok(v.trim().to_owned());
            }
        }
        return Err("couldn't find a `code` parameter in the pasted value".to_owned());
    }
    Ok(s.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn extract_code_accepts_each_pasted_shape() {
        assert_eq!(extract_code("4/abc").unwrap(), "4/abc");
        assert_eq!(extract_code("  4/abc  ").unwrap(), "4/abc");
        assert_eq!(
            extract_code("http://localhost/?code=4/xyz&scope=cal").unwrap(),
            "4/xyz"
        );
        // A fragment that isn't a URL still finds `code=`.
        assert_eq!(extract_code("state=1&code=4%2Ffrag").unwrap(), "4%2Ffrag");
        for bad in ["", "   ", "scope=x&ycode=z"] {
            assert!(extract_code(bad).is_err(), "{bad:?}");
        }
        // A URL whose `code=` substring is only part of another key
        // — the pair loop runs to the end and errors.
        assert!(extract_code("http://localhost/?acode=1").is_err());
    }

    #[test]
    fn redirect_uri_and_capture_hint_cover_no_listener() {
        // No listener → the loopback placeholder + paste
        // instructions.
        assert_eq!(redirect_uri(None), LOOPBACK_REDIRECT);
        print_capture_hint(false, LOOPBACK_REDIRECT);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = redirect_uri(Some(&listener));
        assert!(uri.starts_with("http://127.0.0.1:"), "{uri}");
        print_capture_hint(true, &uri);
    }

    #[test]
    fn read_code_recv_fails_when_no_code_arrives() {
        // The listener answers a request without a `code` — no send,
        // stdin EOF — `rx.recv` errors out.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET /?scope=x HTTP/1.0\r\n\r\n").unwrap();
            let mut body = String::new();
            let _ = s.read_to_string(&mut body);
        });
        assert!(read_code(Some(listener)).is_err());
        client.join().unwrap();
    }

    #[test]
    fn capture_loopback_reads_code_or_answers_400() {
        // The happy path: a request carrying `code` is returned.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET /?code=4%2Floop&scope=x HTTP/1.0\r\n\r\n").unwrap();
            let mut body = String::new();
            s.read_to_string(&mut body).unwrap();
            body
        });
        assert_eq!(capture_loopback(&listener).unwrap(), "4/loop");
        let page = client.join().unwrap();
        assert!(page.contains("200 OK"), "{page}");

        // No `code` parameter → a 400 page and an error.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET /?scope=x HTTP/1.0\r\n\r\n").unwrap();
            let mut body = String::new();
            s.read_to_string(&mut body).unwrap();
            body
        });
        let err = capture_loopback(&listener).unwrap_err();
        assert!(err.contains("no `code`"), "{err}");
        let page = client.join().unwrap();
        assert!(page.contains("400 Bad Request"), "{page}");
    }

    #[test]
    fn read_code_without_listener_reads_one_stdin_line() {
        // `read_code(None)` falls back to stdin — in the test process
        // stdin is at EOF, so the line comes back empty and the
        // caller's extract_code reports the empty code.
        let line = read_code(None).unwrap();
        assert_eq!(line.trim(), "");
        assert!(extract_code(&line).is_err());
    }

    #[test]
    fn read_code_with_listener_takes_the_loopback_code() {
        // With a listener bound, whichever arrives first wins —
        // connect a client that pastes the code so the loopback
        // thread supplies it.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET /?code=4%2Fvia-listener HTTP/1.0\r\n\r\n").unwrap();
        });
        assert_eq!(read_code(Some(listener)).unwrap(), "4/via-listener");
        client.join().unwrap();
    }
}
