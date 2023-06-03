use std::path::Path;
use std::thread;
use std::time::Duration;

use nix::sys::time::TimeSpec;
use nix::time::ClockId;
use thiserror::Error;

const EPOCH_OFFSET: i64 = 2208988800;
const NTP_SERVER: &str = "81.7.16.52:123"; // 0.pool.ntp.org

#[derive(Debug, Error)]
enum Error {
    #[error("nix errno: {0}")]
    NixErrno(#[from] nix::errno::Errno),
    #[error("ntp: {0}")]
    Ntp(#[from] ntp::errors::Error),
}

type Result<T> = std::result::Result<T, Error>;

fn main() -> Result<()> {
    let ip_config = Path::new(rsdsl_ip_config::LOCATION);
    while !ip_config.exists() {
        println!("wait for pppoe");
        thread::sleep(Duration::from_secs(8));
    }

    for i in 0..3 {
        match ntp::request(NTP_SERVER) {
            Ok(resp) => {
                let timespec = TimeSpec::new(resp.transmit_time.sec as i64 - EPOCH_OFFSET, 0);
                println!("server time is {}", timespec);

                nix::time::clock_settime(ClockId::CLOCK_REALTIME, timespec)?;

                println!("set system time");
                break;
            }
            Err(e) => {
                if i == 2 {
                    return Err(e.into());
                }

                thread::sleep(Duration::from_secs(8));
            }
        }
    }

    loop {
        thread::sleep(Duration::MAX);
    }
}
