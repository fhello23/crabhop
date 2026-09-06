mod common;

use axum::{body::Body, http::Request};
use common::setup;
use tower::ServiceExt;

#[tokio::test]
async fn request_logs_do_not_reveal_share_slugs_or_queries() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(output.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    {
        let app = setup().await;
        for path in [
            "/private-share-marker?secret=query-marker",
            "/admin/links/private-share-marker",
            "/unknown/private-share-marker",
        ] {
            let req = Request::builder().uri(path).body(Body::empty()).unwrap();
            app.router.clone().oneshot(req).await.unwrap();
        }
    }
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("response"),
        "capture must contain request logs"
    );
    assert!(!logs.contains("private-share-marker"), "{logs}");
    assert!(!logs.contains("query-marker"), "{logs}");
}
