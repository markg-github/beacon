pub mod avahi;
pub mod uefi;

use std::net::IpAddr;
use std::time::Duration;

use clap::{Parser, Subcommand};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use zbus::Connection;

use tracing::{event, Level};
use tracing::{info, debug, warn, error};
// use tracing_subscriber::{filter, fmt::time, EnvFilter, prelude::*};
use tracing_subscriber::{filter, fmt::time, prelude::*};
// use tracing_subscriber::fmt::time;
// use tracing_subscriber::filter;
// use tracing_subscriber::fmt::time;
// use tracing_subscriber::fmt;
use anyhow::Error;
// use tracing_appender::{non_blocking, rolling};
use tracing_appender::rolling;
use std::io;
use std::path;
use std::env;


use crate::avahi::Avahi;

#[derive(Parser)]
#[command(name = std::env!("CARGO_PKG_NAME"))]
#[command(about = std::env!("CARGO_PKG_DESCRIPTION"))]
struct Cli {
    /// Suppress output to terminal (logs still write to file)
    #[arg(long)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Alerts dispatch that a workload is booting.
    Boot,

    /// Asks dispatch to create a GitHub issue with test results.
    Report {
        /// The title of the GitHub issue
        #[arg(short, long)]
        title: String,

        /// Text file to use as the GitHub issue body
        ///
        /// If not specified, the body will be read from stdin.
        #[arg(short, long, value_name = "FILE")]
        body: Option<std::path::PathBuf>,

        /// Labels for the GitHub issue (can be specified multiple times)
        #[arg(short, long, action = clap::ArgAction::Append)]
        label: Vec<String>,

        /// Assignees for the GitHub issue (can be specified multiple times)
        #[arg(short, long, action = clap::ArgAction::Append)]
        assignee: Vec<String>,

        /// Milestone for the GitHub issue
        #[arg(short, long)]
        milestone: Option<String>,
    },
}

#[derive(Debug, Clone)]
enum Action {
    Boot,
    Report(Report),
}

impl TryFrom<Cli> for Action {
    type Error = Box<dyn std::error::Error>;

    fn try_from(value: Cli) -> Result<Self, Self::Error> {
        match value.command {
            Commands::Boot => Ok(Action::Boot),
            Commands::Report {
                title,
                body,
                label,
                assignee,
                milestone,
            } => Ok(Action::Report(Report {
                title,
                body: body
                    .map(std::fs::read_to_string)
                    .unwrap_or_else(|| std::io::read_to_string(std::io::stdin().lock()))?,
                labels: label,
                assignees: assignee,
                milestone,
            })),
        }
    }
}

impl Action {
    #[tracing::instrument(skip(self, url))]
    async fn perform(&self, url: &str) -> Result<bool, Box<dyn std::error::Error>> {
        debug!(%url, "sending request to dispatch");
        let response = match self {
            Action::Boot => Client::new().post(url).send().await?,
            Action::Report(report) => Client::new().put(url).json(report).send().await?,
        };
        let status = response.status();
        debug!(%url, %status, "received response");
        match status {
            StatusCode::EXPECTATION_FAILED => {
                debug!(%url, "dispatch replied: no task for this IP (wrong instance or no job)");
                Ok(false)
            }
            StatusCode::OK => {
                info!(%url, "dispatch accepted request");
                Ok(true)
            }
            s => {
                warn!(%url, code = ?s, "unexpected status from dispatch");
                Ok(false)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    title: String,

    #[serde(skip_serializing_if = "String::is_empty")]
    body: String,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    assignees: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    milestone: Option<String>,
}

const RESOLVER_TIMEOUT: Duration = Duration::from_secs(5);
const BROWSER_TIMEOUT: Duration = Duration::from_secs(10);

// Avahi D-Bus proxy interfaces
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    // Set up file appender in the system's temp directory
    // let file_appender = tracing_appender::rolling::RollingFileAppender::new(
    //     tracing_appender::rolling::RollingFileAppenderBuilder::new()
    //         .rotation(tracing_appender::rolling::Rotation::DAILY)
    //         .filename_prefix("beacon")
    //         .filename_suffix("log")
    //         .build()
    // );

    // Create subscriber builder with common settings
    // let subscriber = tracing_subscriber::fmt()
    //     .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
    //         .add_directive(tracing::Level::INFO.into())  // Default level
    //     )
    //     .with_thread_ids(true)
    //     .with_target(false)
    //     .with_file(true)
    //     .with_line_number(true)
    //     .with_writer(file_appender);  // Always write to file

    // Parse CLI early to check for --quiet
    let cli = Cli::parse();

    // If not quiet, also write to stdout with less verbose settings
    // if !cli.quiet {
    //     subscriber
    //         .with_writer(std::io::stdout)
    //         .with_file(false)
    //         .with_line_number(false)
    //         .with_thread_ids(false)
    //         .init();
    // } else {
    //     subscriber.init();
    // }

    setup_logging_to_stderr_and_rolling_file("beacon", cli.quiet).unwrap();
    test_tracing_fn();


    let action: Action = cli.try_into()?;

    // let action: Action = Cli::parse().try_into()?;
    info!(?action, "beacon starting");

    let uefi_urls = uefi::find_urls().await?;
    info!(count = uefi_urls.len(), "found uefi-provided URLs");
    for url in uefi_urls {
        info!(%url, "trying UEFI-provided dispatch URL");
        match action.perform(&url).await {
            Ok(true) => {
                info!(%url, "dispatch accepted request");
                return Ok(());
            }
            Ok(false) => {
                debug!(%url, "dispatch did not accept request (no task or wrong instance)");
                continue;
            }
            Err(e) => {
                error!(%url, %e, "error contacting dispatch");
            }
        }
    }

    let connection = Connection::system().await?;
    let avahi = Avahi::new(&connection).await?;

    let mut browsing = avahi.browse(-1, -1, "_dispatch._tcp", "local", 0).await?;
    while let Ok(Some(item)) = timeout(BROWSER_TIMEOUT, browsing.next()).await {
        let resolved = timeout(RESOLVER_TIMEOUT, avahi.resolve(item)).await?;

        match resolved {
            Ok(resolved) => {
                info!(service = %resolved.service.name, address = %resolved.address, "resolved dispatch service");
                match resolved.address.ip() {
                    addr if addr.is_loopback() => continue,
                    IpAddr::V4(ipv4) if ipv4.is_link_local() => continue,
                    IpAddr::V6(ipv6) if ipv6.is_unicast_link_local() => continue,
                    _ => {}
                }

                // Construct the URL
                let url = match resolved.txt.get("path") {
                    Some(path) => format!("http://{}{}", resolved.address, path),
                    None => continue,
                };
                info!(%url, "trying Avahi-discovered dispatch URL");
                match action.perform(&url).await {
                    Ok(true) => {
                        info!(%url, "dispatch accepted request");
                        std::process::exit(0);
                    }
                    Ok(false) => {
                        debug!(%url, "dispatch did not accept request (no task or wrong instance)");
                        continue;
                    }
                    Err(e) => {
                        error!(%url, %e, "error contacting dispatch");
                    }
                }
            }
            Err(e) => warn!(%e, "Avahi resolve failed")
        }
    }

    error!("no dispatch services found");
    Err("no dispatch services found".into())
}

pub fn setup_logging_to_stderr_and_rolling_file(
    filename_prefix: &str,
    quiet: bool,
) -> Result<(), Error> {
    let stderr_log_level = filter::LevelFilter::INFO;
    // let stderr_layer = tracing_subscriber::fmt::layer()
    //     .pretty()
    //     .with_writer(io::stderr);

    let tmp_dir = get_tmp_dir();

    let file_layer = tracing_subscriber::fmt::layer().pretty().with_writer(
        rolling::RollingFileAppender::builder()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix(filename_prefix)
            .filename_suffix("log")
            .build(&tmp_dir)?,
    );

    // Build the registry conditionally including the stderr layer.
    // Build a stderr layer that is either disabled (quiet) or writes to stderr.
    let stderr_layer = if quiet {
        // disabled layer with OFF filter
        tracing_subscriber::fmt::layer()
            .pretty()
            .with_writer(io::stderr)
            .with_timer(time::ChronoLocal::rfc_3339())
            .with_filter(filter::LevelFilter::OFF)
    } else {
        tracing_subscriber::fmt::layer()
            .pretty()
            .with_writer(io::stderr)
            .with_timer(time::ChronoLocal::rfc_3339())
            .with_filter(stderr_log_level)
    };

    // Attach timer and filtering to the file layer and compose the subscriber.
    let registry = tracing_subscriber::registry()
        .with(stderr_layer)
        .with(
            file_layer
                .with_timer(time::ChronoLocal::rfc_3339())
                .with_ansi(false)
                .with_filter(filter::LevelFilter::DEBUG),
        );

    registry.try_init()?;

    let log_dir_abs_path = match path::Path::new(&tmp_dir).canonicalize() {
        Ok(v) => v,
        Err(_) => path::PathBuf::from(tmp_dir),
    };

    event!(Level::INFO, "log dir = {}", log_dir_abs_path.display());

    Ok(())
}

#[tracing::instrument(level = tracing::Level::INFO)]
fn test_tracing_fn() {
    tracing::trace!("This is a trace message");
    tracing::debug!("This is a debug message");
    tracing::info!("This is an info message");
    tracing::warn!("This is a warning message");
    tracing::error!("This is an error message");
}

fn get_tmp_dir() -> String {
    match env::var("TMPDIR").or_else(|_| env::var("TEMP")) {
        Ok(v) => v,
        Err(_) => "log".into(),
    }
}
