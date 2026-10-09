//! A fake HTTP server for unit tests.

/// Serve `router` on an ephemeral loopback port for the rest of the test; returns its base URL.
pub(crate) async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}
