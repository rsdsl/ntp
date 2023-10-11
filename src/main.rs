use std::io;
use std::net::{self, IpAddr, SocketAddr};
use std::path::Path;
use std::thread;
use std::time::Duration;

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

    #[error("nix errno: {0}")]
    NixErrno(#[from] nix::errno::Errno),
    #[error("ntp: {0}")]
    Ntp(#[from] ntp::errors::Error),
    #[error("trust_dns_resolver resolve error: {0}")]
    TrustDnsResolve(#[from] trust_dns_resolver::error::ResolveError),
}

type Result<T> = std::result::Result<T, Error>;

fn main() -> Result<()> {
    let ds_config = Path::new(rsdsl_ip_config::LOCATION);
    while !ds_config.exists() {
        println!("wait for pppoe");
        thread::sleep(Duration::from_secs(8));
    }

    loop {
        match sync_time(NTP_SERVER) {
            Ok(_) => {}
            Err(e) => eprintln!("can't synchronize system time: {}", e),
        }

        thread::sleep(INTERVAL);
    }
}

fn sync_time(server: &str) -> Result<()> {
    let dns = DNS_SERVER.parse()?;
    let server_resolved = SocketAddr::new(resolve_custom_dns(server, dns)?, NTP_PORT);

    let time = ntp::request(server_resolved)?.transmit_time;

    let timespec = TimeSpec::new(time.sec as i64 - EPOCH_OFFSET, 0);
    nix::time::clock_settime(ClockId::CLOCK_REALTIME, timespec)?;

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
