use std::net::{self, IpAddr, SocketAddr};
use std::time::{self, Duration, SystemTime};
use std::{array, io, num};

use tokio::fs;
use tokio::signal::unix::{signal, SignalKind};

use chrono::DateTime;
use hickory_resolver::config::{NameServerConfig, Protocol, ResolverConfig, ResolverOpts};
use hickory_resolver::AsyncResolver;
use nix::sys::time::TimeSpec;
use nix::time::ClockId;
use rsdsl_netlinklib::Connection;
use sysinfo::{ProcessExt, Signal, System, SystemExt};
use thiserror::Error;

const EPOCH_OFFSET: i64 = 2208988800;
const LAST_UNIX_PATH: &str = "/data/ntp.last_unix";
const NTP_SERVER: &str = "2.pool.ntp.org";
const NTP_PORT: u16 = 123;
const DNS_SERVER: &str = "[2620:fe::fe]:53";
const INITIAL_INTERVAL: Duration = Duration::from_secs(30);
const INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Debug, Error)]
enum Error {
    #[error("can't find ntp server hostname")]
    NoHostname,

    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("can't parse network address: {0}")]
    ParseAddr(#[from] net::AddrParseError),
    #[error("system time monotonicity error: {0}")]
    SystemTime(#[from] time::SystemTimeError),
    #[error("integer doesn't fit: {0}")]
    TryFromInt(#[from] num::TryFromIntError),
    #[error("slice length does not equal array length: {0}")]
    TryFromSlice(#[from] array::TryFromSliceError),

    #[error("can't parse (build) timestamp using chrono: {0}")]
    ChronoParse(#[from] chrono::ParseError),
    #[error("nix errno: {0}")]
    NixErrno(#[from] nix::errno::Errno),
    #[error("ntp error: {0}")]
    Ntp(#[from] ntp::errors::Error),
    #[error("hickory_resolver resolve error: {0}")]
    HickoryResolve(#[from] hickory_resolver::error::ResolveError),
    #[error("netlinklib error: {0}")]
    Netlinklib(#[from] rsdsl_netlinklib::Error),
}

type Result<T> = std::result::Result<T, Error>;

#[tokio::main]
async fn main() -> Result<()> {
    println!("init");

    match disk_to_sys().await {
        Ok(_) => println!("load system time"),
        Err(e) => eprintln!("can't load system time: {}", e),
    }

    println!("wait for pppoe");

    let conn = Connection::new().await?;
    conn.link_wait_up("ppp0".into()).await?;

    let mut resync = tokio::time::interval(INITIAL_INTERVAL);
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = resync.tick() => match sync_time(NTP_SERVER).await {
                Ok(_) => {
                    resync = tokio::time::interval(INTERVAL);

                    for dhcp6 in System::new_all().processes_by_exact_name("rsdsl_dhcp6") {
                        dhcp6.kill_with(Signal::User2);
                    }
                }
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
        fs::read(LAST_UNIX_PATH).await.ok()?[..8].try_into().ok()?,
    ))
}

async fn sysnow_to_disk() -> Result<()> {
    let t: i64 = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs()
        .try_into()?;
    fs::write(LAST_UNIX_PATH, t.to_be_bytes()).await?;

    Ok(())
}

async fn disk_to_sys() -> Result<()> {
    let t = i64::from_be_bytes(fs::read(LAST_UNIX_PATH).await?[..8].try_into()?);
    let timespec = TimeSpec::new(t, 0);

    nix::time::clock_settime(ClockId::CLOCK_REALTIME, timespec)?;

    Ok(())
}

async fn sync_time(server: &str) -> Result<()> {
    let last = last_time_unix()
        .await
        .unwrap_or(DateTime::parse_from_rfc3339(env!("SOURCE_TIMESTAMP"))?.timestamp());

    let dns = DNS_SERVER.parse()?;
    let server_resolved = SocketAddr::new(resolve_custom_dns(server, dns).await?, NTP_PORT);

    let time = ntp::request(server_resolved)?.transmit_time;

    let mut t = time.sec as i64 - EPOCH_OFFSET;
    while t < last {
        t += 2_i64.pow(32); // NTP era duration.
    }

    let timespec = TimeSpec::new(t, 0);
    nix::time::clock_settime(ClockId::CLOCK_REALTIME, timespec)?;

    fs::write(LAST_UNIX_PATH, t.to_be_bytes()).await?;

    println!("set system time");
    Ok(())
}

async fn resolve_custom_dns(hostname: &str, custom_dns: SocketAddr) -> Result<IpAddr> {
    let mut cfg = ResolverConfig::new();

    cfg.add_name_server(NameServerConfig::new(custom_dns, Protocol::Udp));

    let resolver = AsyncResolver::tokio(cfg, ResolverOpts::default());
    let response = resolver.lookup_ip(hostname).await?;

    let ip_addr = response.iter().next().ok_or(Error::NoHostname)?;
    Ok(ip_addr)
}
