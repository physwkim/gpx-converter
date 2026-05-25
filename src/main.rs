//! GPX -> TCX 변환 웹서버 (axum).
//!
//! VPN 안에서 혼자 쓰는 자체 호스팅 변환기. 휴대폰에서 페이지를 열고 GPX를
//! 업로드하면 서버가 TCX Course로 변환해 첨부(attachment)로 돌려주므로 바로
//! 다운로드된다. 변환 로직은 [`convert`] 모듈.

mod convert;

use axum::{
    extract::{DefaultBodyLimit, Multipart},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

/// 업로드 본문 최대 크기(대형 경로 대비). 기본 2MB로는 부족할 수 있다.
const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// 기본 포트(env `PORT`로 재정의 가능).
const DEFAULT_PORT: u16 = 8080;

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="ko">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>GPX → TCX 변환</title>
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
  <h1>GPX → TCX 변환</h1>
  <p>GPX 경로 파일을 올리면 사이클링 컴퓨터용 TCX 코스 파일로 변환되어 바로 내려받아집니다.</p>
  <form method="post" action="/convert" enctype="multipart/form-data">
    <input type="file" name="file" accept=".gpx,application/gpx+xml" required>
    <button type="submit">변환 &amp; 다운로드</button>
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
        .unwrap_or_else(|e| panic!("{addr} 바인드 실패: {e}"));
    println!("gpx-converter listening on http://{addr}");
    axum::serve(listener, app).await.expect("서버 실행 실패");
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
                            return (StatusCode::BAD_REQUEST, format!("업로드 읽기 실패: {e}"))
                                .into_response()
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("multipart 오류: {e}")).into_response()
            }
        }
    }

    let Some(bytes) = file_bytes else {
        return (
            StatusCode::BAD_REQUEST,
            "파일이 없습니다. GPX 파일을 선택하세요.".to_string(),
        )
            .into_response();
    };

    match convert::gpx_to_tcx(&bytes, &file_name) {
        Ok(tcx) => {
            let download = format!("{}.tcx", tcx.filename);
            // 한글 파일명은 RFC 5987 filename*로 전달. ASCII 폴백도 함께 둔다.
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
