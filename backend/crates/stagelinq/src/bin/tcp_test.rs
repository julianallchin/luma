//! TCP connection experiments against StageLinQ device.
//! cargo run -p stagelinq --bin tcp-test

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

fn main() {
    let addr = "169.254.210.143:46525";

    // Start UDP announcements in a background thread
    eprintln!("Starting UDP announcements...");
    let announce_thread = std::thread::spawn(|| {
        use std::net::UdpSocket;

        // Announce socket (ephemeral port, like node lib)
        let sock = UdpSocket::bind("0.0.0.0:0").unwrap();
        sock.set_broadcast(true).unwrap();
        eprintln!("  Announce from {:?}", sock.local_addr());

        let token: [u8; 16] = [
            0x52, 0xFD, 0xFC, 0x07, 0x21, 0x82, 0x65, 0x4F, 0x16, 0x3F, 0x5F, 0x0F, 0x9A, 0x62,
            0x1D, 0x72,
        ];

        // Build announcement manually (same as stagelinq crate)
        let msg = stagelinq::protocol::build_discovery_message(
            &token,
            "np2",
            "DISCOVERER_HOWDY_",
            "nowplaying",
            "2.2.0",
            0,
        );

        let dest: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, 51337));
        loop {
            let _ = sock.send_to(&msg, dest);
            std::thread::sleep(Duration::from_millis(1000));
        }
    });

    // Also bind the listener socket on 51337 (like node lib does)
    let _listener = std::net::UdpSocket::bind("0.0.0.0:51337").unwrap();
    eprintln!("Listener bound to 51337");

    // Wait for announcements to propagate
    eprintln!();
    eprintln!("=== Waiting 8 seconds for announcements to reach device ===");
    std::thread::sleep(Duration::from_secs(8));

    // Now try TCP connects
    for attempt in 1..=8 {
        eprintln!();
        eprintln!("=== TCP attempt {attempt}/8 ===");

        let mut stream =
            match TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_secs(5)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  Connect failed: {e}");
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };
        eprintln!("  Connected from {:?}", stream.local_addr());

        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();

        let start = Instant::now();
        let mut buf = [0u8; 4096];
        match stream.read(&mut buf) {
            Ok(0) => {
                eprintln!("  Read 0 after {:?} - peer closed", start.elapsed());
            }
            Ok(n) => {
                eprintln!("  *** GOT {n} BYTES after {:?}! ***", start.elapsed());
                for (i, chunk) in buf[..n].chunks(16).enumerate() {
                    let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                    let ascii: String = chunk
                        .iter()
                        .map(|&b| {
                            if (0x20..=0x7e).contains(&b) {
                                b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    eprintln!("    {:04x}: {:<48} {}", i * 16, hex.join(" "), ascii);
                }
                eprintln!("  SUCCESS! Device is responding.");
                break;
            }
            Err(e) => {
                eprintln!("  Read error after {:?}: {e}", start.elapsed());
            }
        }

        // Wait between attempts (node lib effectively waits ~5s due to requestAllServicePorts timeout)
        eprintln!("  Waiting 5s before next attempt...");
        std::thread::sleep(Duration::from_secs(5));
    }

    eprintln!();
    eprintln!("Done.");
    drop(announce_thread);
}
