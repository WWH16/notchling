//! The HTTP client shared by every web request the notch makes.

use std::time::Duration;

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// A slower request is dropped, so the polling thread moves on.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// An HTTPS-only client using Windows' own TLS and certificate store, so corporate
/// proxies' roots work too.
#[must_use]
pub fn agent() -> Agent {
    let tls = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .timeout_global(Some(REQUEST_TIMEOUT))
        .user_agent(concat!("notchling/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}
