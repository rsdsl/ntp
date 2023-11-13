use std::io;
use std::net::{self, IpAddr, SocketAddr};
use std::num;
use std::path::Path;
use std::thread;
use std::time::{self, Duration, SystemTime};

use tokio::fs;
use tokio::signal::unix::{signal, SignalKind};

use chrono::DateTime;
use nix::sys::time::TimeSpec;
use nix::time::ClockId;
use thiserror::Error;
use trust_dns_resolver::config::{NameServerConfig, Protocol, ResolverConfig, ResolverOpts};
use trust_dns_resolver::Resolver;

const EPOCH_OFFSET: i64 = 2208988800;
const NTP_SERVER: &str = "2.pool.ntp.org";
const NTP_PORT: u16 = 123;
const DNS_SERVER: &str = "[2620:fe::fe]:53";
const INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Debug, Error)]
enum Error {
    #[error("can't find ntp server hostname")]
    NoHostname,

    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("can't parse network address: {0}")]
    ParseAddr(#[from] net::AddrParseError),
    #[error("system time monotonicity error: {0}")]
    SystemTime(#[from] time::SystemTimeError),
    #[error("integer doesn't fit: {0}")]
    TryFromInt(#[from] num::TryFromIntError),

    #[error("chrono parse: {0}")]
    ChronoParse(#[from] chrono::ParseError),
    #[error("nix errno: {0}")]
    NixErrno(#[from] nix::errno::Errno),
    #[error("ntp: {0}")]
    Ntp(#[from] ntp::errors::Error),
    #[error("trust_dns_resolver resolve error: {0}")]
    TrustDnsResolve(#[from] trust_dns_resolver::error::ResolveError),
}

type Result<T> = std::result::Result<T, Error>;

#[tokio::main]
async fn main() -> Result<()> {
    let ds_config = Path::new(rsdsl_ip_config::LOCATION);
    while !ds_config.exists() {
        println!("wait for pppoe");
        thread::sleep(Duration::from_secs(8));
    }

    let mut resync = tokio::time::interval(INTERVAL);
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = resync.tick() => match sync_time(NTP_SERVER).await {
                Ok(_) => {}
                Err(e) => eprintln!("can't synchronize system time: {}", e),
            },
            _ = sigterm.recv() => {
                sysnow_to_disk().await?;

                println!("save system time");
                return Ok(());
            }
        }
    }
}

async fn last_time_unix() -> Option<i64> {
    Some(i64::from_be_bytes(
        fs::read("/data/ntp.last_unix").await.ok()?[..8]
            .try_into()
            .ok()?,
    ))
}

async fn sysnow_to_disk() -> Result<()> {
    let t: i64 = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs()
        .try_into()?;
    fs::write("/data/ntp.last_unix", t.to_be_bytes()).await?;

    Ok(())
}

async fn sync_time(server: &str) -> Result<()> {
    let last = last_time_unix()
        .await
        .unwrap_or(DateTime::parse_from_rfc3339(env!("SOURCE_TIMESTAMP"))?.timestamp());

    let dns = DNS_SERVER.parse()?;
    let server_resolved = SocketAddr::new(resolve_custom_dns(server, dns)?, NTP_PORT);

    let time = ntp::request(server_resolved)?.transmit_time;

    let mut t = time.sec as i64 - EPOCH_OFFSET;
    while t < last {
        t += 2_i64.pow(32); // NTP era duration.
    }

    let timespec = TimeSpec::new(t, 0);
    nix::time::clock_settime(ClockId::CLOCK_REALTIME, timespec)?;

    fs::write("/data/ntp.last_unix", t.to_be_bytes()).await?;

    println!("set system time");
    Ok(())
}

fn resolve_custom_dns(hostname: &str, custom_dns: SocketAddr) -> Result<IpAddr> {
    let mut cfg = ResolverConfig::new();

    cfg.add_name_server(NameServerConfig::new(custom_dns, Protocol::Udp));

    let resolver = Resolver::new(cfg, ResolverOpts::default())?;
    let response = resolver.lookup_ip(hostname)?;

    let ip_addr = response.iter().next().ok_or(Error::NoHostname)?;
    Ok(ip_addr)
}
