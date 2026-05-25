//! GPX -> TCX conversion web server (axum).
//!
//! A self-hosted converter for personal use behind a VPN. Open the page on your
//! phone, upload a GPX, and the server converts it to a TCX Course and returns
//! it as an attachment so it downloads immediately. See the [`convert`] module.

mod convert;

use axum::{
    extract::{DefaultBodyLimit, Multipart},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

/// Max upload body size (for large routes). The default 2 MB can be too small.
const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// Default port (overridable via the `PORT` env var).
const DEFAULT_PORT: u16 = 8080;

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>GPX → TCX Converter</title>
<style>
  :root { color-scheme: light dark; }
  body { font-family: system-ui, -apple-system, sans-serif; margin: 0;
         min-height: 100vh; display: grid; place-items: center; padding: 1.5rem; }
  main { width: 100%; max-width: 28rem; }
  h1 { font-size: 1.4rem; margin: 0 0 .25rem; }
  p { color: #666; margin: 0 0 1.25rem; line-height: 1.5; }
  form { display: flex; flex-direction: column; gap: 1rem; }
  input[type=file] { padding: .9rem; border: 1px dashed #999; border-radius: .6rem;
                     width: 100%; box-sizing: border-box; font-size: 1rem; }
  button { padding: .9rem 1rem; font-size: 1.05rem; border: 0; border-radius: .6rem;
           background: #2563eb; color: #fff; cursor: pointer; }
  button:active { background: #1d4ed8; }
</style>
</head>
<body>
<main>
  <h1>GPX → TCX Converter</h1>
  <p>Upload a GPX route file and it is converted to a TCX course file for your cycling computer and downloaded right away.</p>
  <form method="post" action="/convert" enctype="multipart/form-data">
    <input type="file" name="file" accept=".gpx,application/gpx+xml" required>
    <button type="submit">Convert &amp; Download</button>
  </form>
</main>
</body>
</html>
"#;

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(index))
        .route("/convert", post(convert_handler))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES));

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    println!("gpx-converter listening on http://{addr}");
    axum::serve(listener, app).await.expect("server failed");
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn convert_handler(mut multipart: Multipart) -> Response {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name = String::from("route.gpx");

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if field.name() == Some("file") {
                    if let Some(fname) = field.file_name() {
                        if !fname.is_empty() {
                            file_name = fname.to_string();
                        }
                    }
                    match field.bytes().await {
                        Ok(b) => file_bytes = Some(b.to_vec()),
                        Err(e) => {
                            return (
                                StatusCode::BAD_REQUEST,
                                format!("failed to read upload: {e}"),
                            )
                                .into_response()
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("multipart error: {e}")).into_response()
            }
        }
    }

    let Some(bytes) = file_bytes else {
        return (
            StatusCode::BAD_REQUEST,
            "No file. Please choose a GPX file.".to_string(),
        )
            .into_response();
    };

    match convert::gpx_to_tcx(&bytes, &file_name) {
        Ok(tcx) => {
            let download = format!("{}.tcx", tcx.filename);
            // Non-ASCII filenames (e.g. Korean) go via RFC 5987 filename*; an ASCII fallback is kept too.
            let encoded = utf8_percent_encode(&download, NON_ALPHANUMERIC).to_string();
            let disposition =
                format!("attachment; filename=\"route.tcx\"; filename*=UTF-8''{encoded}");
            (
                StatusCode::OK,
                [
                    (
                        header::CONTENT_TYPE,
                        "application/vnd.garmin.tcx+xml; charset=utf-8".to_string(),
                    ),
                    (header::CONTENT_DISPOSITION, disposition),
                ],
                tcx.xml,
            )
                .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}
