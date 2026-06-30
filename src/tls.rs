//! Automatic TLS via Let's Encrypt (ACME, TLS-ALPN-01 on :443).
//!
//! The certificate domain set is sourced from the `domains` table, so adding a
//! domain in the dashboard makes the server reload and provision a cert for it
//! (once its DNS points here) without a restart. On :80, requests for a
//! configured domain are redirected to HTTPS while other hosts (e.g. the raw IP)
//! are still served, so adding a domain can't lock the operator out; the
//! TLS-ALPN-01 challenge is answered on 443.

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    Router,
};
use futures::StreamExt;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rustls_acme::{caches::DirCache, AcmeConfig};
use tokio::net::TcpListener;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use crate::db;
use crate::state::AppState;

/// Serve the app over HTTPS with automatic certificates, plus an HTTP→HTTPS
/// redirector. Returns only on fatal error.
pub async fn serve(state: AppState, app: Router) -> anyhow::Result<()> {
    // rustls needs a process-wide crypto provider.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let http_addr = state.config().server.http_addr.clone();
    let https_addr = state.config().server.https_addr.clone();
    let public_url = state.config().server.public_url.clone();

    loop {
        let domains = db::domains::allowlist(state.db()).await.unwrap_or_default();
        if domains.is_empty() {
            // Automatic TLS is on but there's no domain to certify yet. Serve the
            // dashboard over plain HTTP so it stays reachable (e.g. by IP) for
            // first-run setup, and switch to TLS the moment a domain is added —
            // redirecting :80 now would strand the box on an unservable :443.
            tracing::warn!(
                %http_addr,
                "automatic TLS is on but no domain is configured yet — serving HTTP; add a domain in the dashboard to provision a certificate"
            );
            tokio::select! {
                res = crate::server::serve_http(&http_addr, app.clone()) => return res,
                _ = state.domains_changed() => {
                    tracing::info!("domain added — provisioning TLS");
                }
            }
            continue;
        }

        // A domain is configured: on :80 redirect *configured-domain* hosts to
        // HTTPS but keep serving the app for everything else (e.g. the raw IP),
        // so adding a domain whose DNS/cert isn't ready can't lock the operator
        // out. Serve ACME-managed certificates on :443.
        tracing::info!(?domains, "starting ACME TLS listener");
        let redirect = tokio::spawn(http_server(
            http_addr.clone(),
            app.clone(),
            domains.clone(),
            public_url.clone(),
        ));
        tokio::select! {
            res = run_acme(&state, &https_addr, app.clone(), domains) => {
                if let Err(e) = res {
                    tracing::error!(error = %e, "ACME listener failed; retrying in 5s");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
            _ = state.domains_changed() => {
                tracing::info!("domain set changed — reloading TLS");
            }
        }
        // Drop the :80 redirector before the next iteration rebinds it (or falls
        // back to serving HTTP if every domain was removed).
        redirect.abort();
    }
}

#[allow(deprecated)] // low-level acceptor API is stable enough for our loop
async fn run_acme(
    state: &AppState,
    addr: &str,
    app: Router,
    domains: Vec<String>,
) -> anyhow::Result<()> {
    let tls = state.config().tls.clone();
    // Contact email is editable live in the dashboard (DB wins over config).
    let contact = db::settings::effective_contact_email(state.db(), &tls.contact_email)
        .await
        .unwrap_or_else(|_| tls.contact_email.clone());
    let mut builder = AcmeConfig::new(domains);
    if !contact.is_empty() {
        builder = builder.contact_push(format!("mailto:{contact}"));
    }
    let mut acme = builder
        .cache(DirCache::new(tls.cache_dir.clone()))
        .directory_lets_encrypt(!tls.staging)
        .state();
    let acceptor = acme.acceptor();

    // rustls config for normal traffic: serve the ACME-managed certs via the
    // state's resolver, negotiating HTTP/2 then HTTP/1.1.
    let mut server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(acme.resolver());
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let server_config = std::sync::Arc::new(server_config);

    // Drive ACME order processing in the background.
    let db_for_events = state.db().clone();
    tokio::spawn(async move {
        loop {
            match acme.next().await {
                Some(Ok(ok)) => tracing::info!("acme: {:?}", ok),
                Some(Err(err)) => {
                    tracing::error!("acme error: {:?}", err);
                    let _ = &db_for_events; // status updates wired via dashboard polling
                }
                None => break,
            }
        }
    });

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "ruskery listening (https)");
    loop {
        let (tcp, _peer) = listener.accept().await?;
        let accept = acceptor.clone();
        let app = app.clone();
        let server_config = server_config.clone();
        tokio::spawn(async move {
            // rustls-acme works over futures-io; bridge to/from tokio-io.
            match accept.accept(tcp.compat()).await {
                Ok(Some(handshake)) => match handshake.into_stream(server_config).await {
                    Ok(tls_stream) => {
                        let io = TokioIo::new(tls_stream.compat());
                        let svc = TowerToHyperService::new(app);
                        if let Err(e) = Builder::new(TokioExecutor::new())
                            .serve_connection_with_upgrades(io, svc)
                            .await
                        {
                            tracing::debug!("connection closed: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("tls handshake error: {e}"),
                },
                Ok(None) => { /* ACME TLS-ALPN-01 challenge handled internally */ }
                Err(e) => tracing::debug!("tls accept error: {e}"),
            }
        });
    }
}

/// Plain-HTTP server for when domains are configured: requests whose `Host` is a
/// configured domain are 308-redirected to HTTPS; all other hosts (e.g. the raw
/// IP) are served the app normally, so the operator can't be locked out before a
/// domain's DNS/cert is ready.
async fn http_server(addr: String, app: Router, domains: Vec<String>, public_url: String) {
    let domain_set: std::collections::HashSet<String> = domains.into_iter().collect();
    let app = app.layer(axum::middleware::from_fn(
        move |req: Request, next: Next| {
            let domains = domain_set.clone();
            let public_url = public_url.clone();
            async move {
                let host = req
                    .headers()
                    .get(header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .map(|h| h.split(':').next().unwrap_or(h).to_string());
                match host {
                    Some(h) if domains.contains(&h) => redirect_to_https(req, &public_url),
                    _ => next.run(req).await,
                }
            }
        },
    ));
    match TcpListener::bind(&addr).await {
        Ok(listener) => {
            tracing::info!(%addr, "ruskery listening (http; configured domains redirect to https)");
            let svc = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
            if let Err(e) = axum::serve(listener, svc).await {
                tracing::error!(error = %e, "http server failed");
            }
        }
        Err(e) => tracing::error!(error = %e, %addr, "failed to bind http listener"),
    }
}

/// Build the HTTPS redirect target. Prefer the configured `public_url` (no
/// open redirect via a spoofed `Host`); fall back to the request host only when
/// no public URL is set (e.g. local dev).
fn redirect_to_https(req: Request, public_url: &str) -> Response {
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");

    if !public_url.is_empty() {
        let base = public_url.trim_end_matches('/');
        return Redirect::permanent(&format!("{base}{path}")).into_response();
    }

    match req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_string())
    {
        Some(h) => Redirect::permanent(&format!("https://{h}{path}")).into_response(),
        None => (StatusCode::BAD_REQUEST, "missing host").into_response(),
    }
}
