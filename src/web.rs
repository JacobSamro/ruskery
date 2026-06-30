//! Serve the embedded single-page dashboard. Static assets are matched by path;
//! anything else falls back to `index.html` so client-side routing works.
//!
//! The dashboard is a Nuxt SPA, whose `index.html` carries a tiny inline
//! bootstrap script (`window.__NUXT__ = …`). To keep a strict
//! `script-src 'self'` (no `'unsafe-inline'`), we hash every inline script in
//! the embedded HTML at startup and emit those `'sha256-…'` tokens in the CSP
//! we set on HTML responses. The hashes are derived from the bytes we actually
//! ship, so they stay correct across rebuilds (Nuxt's `buildId` changes each
//! build, which would break any hard-coded hash).

use std::sync::OnceLock;

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/.output/public"]
struct Assets;

/// Fallback handler for all non-API, non-registry routes.
pub async fn handler(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    if let Some(resp) = serve(path) {
        return resp;
    }
    // SPA fallback.
    serve("index.html").unwrap_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "dashboard assets not built (run the web build)",
        )
            .into_response()
    })
}

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let is_html = mime.essence_str() == "text/html";
    let body = Body::from(file.data.into_owned());
    let mut resp = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, mime.as_ref())],
        body,
    )
        .into_response();
    // HTML documents carry inline scripts, so they need a CSP whose script-src
    // whitelists those scripts by hash. Other responses inherit the strict
    // default CSP from the global middleware layer.
    if is_html {
        resp.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(html_csp()).expect("html CSP is valid header value"),
        );
    }
    Some(resp)
}

/// Convenience used by the router fallback when only the URI is available.
#[allow(dead_code)]
pub async fn handler_uri(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    serve(path)
        .or_else(|| serve("index.html"))
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "not found").into_response())
}

/// The CSP applied to HTML responses: the strict default plus `'sha256-…'`
/// tokens for each inline script in the embedded `index.html`.
fn html_csp() -> &'static str {
    static HTML_CSP: OnceLock<String> = OnceLock::new();
    HTML_CSP.get_or_init(|| {
        let hashes = Assets::get("index.html")
            .map(|f| inline_script_hashes(&String::from_utf8_lossy(&f.data)))
            .unwrap_or_default();
        csp_policy(&hashes)
    })
}

/// Build the full Content-Security-Policy. `script_hashes` are appended to
/// `script-src` (each already in `sha256-<base64>` form). With no hashes this
/// is the strict default used for every non-HTML response.
pub fn csp_policy(script_hashes: &[String]) -> String {
    let mut script_src = String::from("script-src 'self'");
    for h in script_hashes {
        script_src.push_str(" '");
        script_src.push_str(h);
        script_src.push('\'');
    }
    format!(
        "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; \
         {script_src}; connect-src 'self'; base-uri 'self'; frame-ancestors 'none'"
    )
}

/// Extract `'sha256-<base64>'` tokens for every *executable* inline script in
/// the document. Scripts that load an external resource (a real `src` attribute,
/// already covered by `'self'`) and non-executable data blocks (e.g.
/// `type="application/json"` payloads) are skipped — they are not subject to
/// script-execution policy.
///
/// HTML tag/attribute names are matched ASCII-case-insensitively, the start tag
/// is scanned with quote awareness (so a `>` inside an attribute value doesn't
/// truncate it), and `src`/`type` are matched as real attributes rather than as
/// substrings — so `data-src`/`data-type` don't fool it. The hash is taken over
/// the exact script text content, matching what a browser computes for CSP.
fn inline_script_hashes(html: &str) -> Vec<String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = find_ci(&html[pos..], "<script") {
        let start = pos + rel;
        let after_name = start + "<script".len();
        // A real start tag is followed by whitespace, `>`, or `/` — otherwise
        // it's some other element (e.g. `<scriptlet>`); skip just the match.
        let boundary = html[after_name..].chars().next();
        if !matches!(boundary, Some(c) if c.is_ascii_whitespace() || c == '>' || c == '/') {
            pos = after_name;
            continue;
        }
        let Some(tag_len) = tag_end(&html[after_name..]) else {
            break;
        };
        let attrs_str = &html[after_name..after_name + tag_len];
        let body_start = after_name + tag_len + 1;
        // Closing tag, matched case-insensitively (`</script` then its `>`).
        let Some(close_rel) = find_ci(&html[body_start..], "</script") else {
            break;
        };
        let body = &html[body_start..body_start + close_rel];
        let after_close = body_start + close_rel;
        pos = match html[after_close..].find('>') {
            Some(g) => after_close + g + 1,
            None => break,
        };

        // `<script .../>` self-closes with no body.
        if attrs_str.trim_end().ends_with('/') {
            continue;
        }
        let attrs = parse_attrs(attrs_str);
        if attrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("src")) {
            continue;
        }
        let ty = attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("type"))
            .map(|(_, v)| v.as_str());
        if !type_is_executable(ty) || body.is_empty() {
            continue;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(body.as_bytes()));
        out.push(format!("sha256-{b64}"));
    }
    out
}

/// ASCII-case-insensitive substring search, returning the start index in `hay`.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() {
        return Some(0);
    }
    h.windows(n.len())
        .position(|w| w.iter().zip(n).all(|(a, b)| a.eq_ignore_ascii_case(b)))
}

/// Index of the `>` that ends a start tag, skipping over quoted attribute
/// values so a `>` inside a value isn't mistaken for the tag end.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, &c) in s.as_bytes().iter().enumerate() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'>' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

/// Parse a start tag's attribute text into `(name, value)` pairs, handling
/// `name`, `name=value`, `name="value"`, and `name='value'` forms.
fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut attrs = Vec::new();
    while i < n {
        while i < n && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let name_start = i;
        while i < n && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' && b[i] != b'>' {
            i += 1;
        }
        if i == name_start {
            i += 1; // stray char (e.g. lone '>'); advance to make progress
            continue;
        }
        let name = s[name_start..i].to_string();
        while i < n && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < n && b[i] == b'=' {
            i += 1;
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < n && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let v_start = i;
                while i < n && b[i] != q {
                    i += 1;
                }
                value = s[v_start..i].to_string();
                i += 1; // skip closing quote (saturates at n)
            } else {
                let v_start = i;
                while i < n && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                value = s[v_start..i].to_string();
            }
        }
        attrs.push((name, value));
    }
    attrs
}

/// Whether a `type` attribute makes the script execute as JS. Per the HTML spec
/// a script runs when its type is absent/empty, `module`, or a JavaScript MIME
/// essence (any `;`-parameters are ignored).
fn type_is_executable(ty: Option<&str>) -> bool {
    let Some(t) = ty.map(str::trim) else {
        return true;
    };
    if t.is_empty() || t.eq_ignore_ascii_case("module") {
        return true;
    }
    let essence = t
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "application/javascript"
            | "application/ecmascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/javascript"
            | "text/ecmascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_only_executable_inline_scripts() {
        let html = r#"<!doctype html><html><head>
            <script type="module" src="/_nuxt/entry.js" crossorigin></script>
            <script id="unhead:payload" type="application/json">{"title":"ruskery"}</script>
            <script>window.__NUXT__={};window.__NUXT__.config={app:{baseURL:"/"}}</script>
            <script type="application/json" id="__NUXT_DATA__">[1,2,3]</script>
            </head><body></body></html>"#;
        let hashes = inline_script_hashes(html);
        // Only the bare inline bootstrap script is hashed.
        assert_eq!(hashes.len(), 1, "got: {hashes:?}");
        assert!(hashes[0].starts_with("sha256-"));

        // The policy must keep 'self' and append the computed hash to script-src.
        let policy = csp_policy(&hashes);
        assert!(policy.contains("script-src 'self' '"));
        assert!(policy.contains(&hashes[0]));
        assert!(policy.contains("frame-ancestors 'none'"));
    }

    #[test]
    fn default_policy_has_no_inline_allowance() {
        let policy = csp_policy(&[]);
        // script-src is exactly 'self' — no inline allowance leaks in.
        assert!(policy.contains("script-src 'self';"));
        assert!(!policy.contains("script-src 'self' 'unsafe-inline'"));
        assert!(!policy.contains("script-src 'self' '"));
    }

    fn sha(body: &[u8]) -> String {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        format!(
            "sha256-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(body))
        )
    }

    #[test]
    fn skips_external_and_data_scripts() {
        // Real src / real type=application/json → not executable inline.
        assert!(inline_script_hashes(r#"<script src="/a.js">x</script>"#).is_empty());
        assert!(inline_script_hashes(r#"<script type="application/json">{}</script>"#).is_empty());
        assert!(inline_script_hashes(r#"<script src="/a.js" />"#).is_empty());
    }

    #[test]
    fn data_attributes_do_not_masquerade_as_src_or_type() {
        // `data-src` is NOT `src`; the script executes and must be hashed.
        let h = inline_script_hashes(r#"<script data-src="/x">window.__NUXT__={}</script>"#);
        assert_eq!(h, vec![sha(b"window.__NUXT__={}")]);
        // `data-type="application/json"` is NOT `type`; still executable.
        let h = inline_script_hashes(r#"<script data-type="application/json">CODE()</script>"#);
        assert_eq!(h, vec![sha(b"CODE()")]);
    }

    #[test]
    fn case_insensitive_tags_and_attrs() {
        assert_eq!(
            inline_script_hashes(r#"<SCRIPT>window.__NUXT__={}</SCRIPT>"#),
            vec![sha(b"window.__NUXT__={}")]
        );
        // Uppercase TYPE=application/json data block → skipped.
        assert!(inline_script_hashes(r#"<script TYPE="application/json">{}</script>"#).is_empty());
    }

    #[test]
    fn quoted_gt_does_not_truncate_the_tag_or_corrupt_the_hash() {
        // A `>` inside an attribute value must not end the start tag early, so
        // the hash is over the true body, not `b">window.__NUXT__={}`.
        let h = inline_script_hashes(r#"<script data-x="a>b">window.__NUXT__={}</script>"#);
        assert_eq!(h, vec![sha(b"window.__NUXT__={}")]);
    }

    #[test]
    fn js_mime_with_parameters_is_executable() {
        let h = inline_script_hashes(
            r#"<SCRIPT TYPE="text/javascript; charset=utf-8">CODE()</SCRIPT>"#,
        );
        assert_eq!(h, vec![sha(b"CODE()")]);
        // `module` is executable too.
        assert_eq!(
            inline_script_hashes(r#"<script type="module">x()</script>"#),
            vec![sha(b"x()")]
        );
    }
}
