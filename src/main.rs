use std::{env, net::ToSocketAddrs};

use log::info;
use serde::Deserialize;
use simple_logger::SimpleLogger;
use warp::{http::StatusCode, Filter};

#[tokio::main]
async fn main() {
    run().await
}

async fn run() {
    SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .init()
        .unwrap();
    let bind_addr = env::var("ULS_EXPORTER_BINDADDR")
        .unwrap_or("0.0.0.0:9837".to_string())
        .to_socket_addrs()
        .expect("failed to parse ULS_EXPORTER_BINDADDR")
        .next()
        .expect("failed to parse ULS_EXPORTER_BINDADDR");

    let uls_base_url = env::var("ULS_BASE_URL").expect("Environment Variable ULS_BASE_URL not set");

    let uls_lease_url = format!("{}/v1/admin/lease", uls_base_url);
    let uls_lease_url = Box::leak(uls_lease_url.into_boxed_str()) as &'static str;
    info!("ULS lease url is {}", uls_lease_url);

    let uls_status_url = format!("{}/v1/admin/status", uls_base_url);
    let uls_status_url = Box::leak(uls_status_url.into_boxed_str()) as &'static str;
    info!("ULS status url is {}", uls_status_url);

    let index =
        warp::path::end().map(|| "Unity License Server Exporter \n Metrics exported on /metrics");
    let metrics = warp::path("metrics")
        .and(warp::path::end())
        .and_then(move || metrics_handle(uls_status_url, uls_lease_url));
    warp::serve(index.or(metrics)).run(bind_addr).await
}

async fn metrics_handle(
    status_endpoint: &str,
    lease_endpoint: &str,
) -> Result<Box<dyn warp::Reply>, warp::Rejection> {
    Ok(match metrics(status_endpoint, lease_endpoint).await {
        Ok(s) => Box::new(s),
        Err(e) => Box::new(warp::reply::with_status(
            format!(
                "# An error occured while trying to contact the license server: \n# {}",
                e.to_string()
                    .split("\n")
                    .collect::<Vec<&str>>()
                    .join("\n# ")
            ),
            StatusCode::SERVICE_UNAVAILABLE,
        )),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EntitlementContext {
    environment_domain: String,
    environment_hostname: String,
    environment_user: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct License {
    floating_lease_id: i32,
    client_entitlement_context: EntitlementContext,
    is_revoked: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusReport {
    server_status: String,
    server_up_time_ms: i64,
}

async fn metrics(status_endpoint: &str, lease_endpoint: &str) -> anyhow::Result<String> {
    use prometheus::{Encoder, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder};

    let r = Registry::new();

    let status_report: StatusReport = reqwest::get(status_endpoint).await?.json().await?;

    let health_gauge = IntGauge::new("uls_health", "Health of the ULS")?;
    let uptime_gauge = IntGauge::new("uls_uptime_ms", "Uptime of the ULS in ms")?;

    r.register(Box::new(health_gauge.clone())).unwrap();
    r.register(Box::new(uptime_gauge.clone())).unwrap();

    health_gauge.set(if status_report.server_status == "Healthy" {
        1
    } else {
        0
    });

    uptime_gauge.set(status_report.server_up_time_ms);

    if status_report.server_status == "Healthy" {
        let report: Vec<License> = reqwest::get(lease_endpoint).await?.json().await?;
        let lease_opts = Opts::new("uls_license_leased", "Currently leased ULS License");

        let lease_gauge = IntGaugeVec::new(
            lease_opts,
            &["lease_id", "lease_user", "lease_hostname", "lease_domain"],
        )?;

        // Create a Registry and register Counter.
        r.register(Box::new(lease_gauge.clone())).unwrap();

        for license in report.iter() {
            lease_gauge
                .with_label_values(&[
                    license.floating_lease_id.to_string().as_str(),
                    &license.client_entitlement_context.environment_user,
                    &license.client_entitlement_context.environment_hostname,
                    &license.client_entitlement_context.environment_domain,
                ])
                .set(if license.is_revoked { 0 } else { 1 });
        }
    }

    // Gather the metrics.
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = r.gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();

    Ok(String::from_utf8(buffer).unwrap())
}
