use super::*;
use std::sync::mpsc::{channel, sync_channel, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

fn streams() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    (server, client)
}

#[cfg(unix)]
fn small_buffer(stream: &TcpStream, send: bool) -> i32 {
    use std::os::fd::AsRawFd;
    let option = if send { libc::SO_SNDBUF } else { libc::SO_RCVBUF };
    let requested = 4096i32;
    let mut length = std::mem::size_of_val(&requested) as libc::socklen_t;
    let result = unsafe {
        libc::setsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, option, (&requested as *const i32).cast(), length)
    };
    assert_eq!(result, 0, "setsockopt: {}", io::Error::last_os_error());
    let mut actual = 0i32;
    let result = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, option, (&mut actual as *mut i32).cast(), &mut length)
    };
    assert_eq!(result, 0, "getsockopt: {}", io::Error::last_os_error());
    actual
}

#[cfg(windows)]
fn small_buffer(stream: &TcpStream, send: bool) -> i32 {
    use std::os::windows::io::AsRawSocket;
    #[link(name = "Ws2_32")]
    extern "system" {
        fn setsockopt(socket: usize, level: i32, option: i32, value: *const std::ffi::c_char, len: i32) -> i32;
        fn getsockopt(socket: usize, level: i32, option: i32, value: *mut std::ffi::c_char, len: *mut i32) -> i32;
        #[link_name = "WSAGetLastError"]
        fn wsa_get_last_error() -> i32;
    }
    let option = if send { 0x1001 } else { 0x1002 };
    let requested = if send { 0i32 } else { 4096i32 };
    let mut length = std::mem::size_of_val(&requested) as i32;
    let result = unsafe {
        setsockopt(stream.as_raw_socket() as usize, 0xffff, option, (&requested as *const i32).cast(), length)
    };
    assert_eq!(result, 0, "setsockopt: {}", unsafe { wsa_get_last_error() });
    let mut actual = 0i32;
    let result = unsafe {
        getsockopt(stream.as_raw_socket() as usize, 0xffff, option, (&mut actual as *mut i32).cast(), &mut length)
    };
    assert_eq!(result, 0, "getsockopt: {}", unsafe { wsa_get_last_error() });
    actual
}

struct BurstRead {
    bytes: Vec<u8>,
    progress: Vec<Instant>,
}

fn burst_reader(mut client: TcpStream) -> (Sender<()>, JoinHandle<BurstRead>) {
    client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let (stop, control) = channel();
    let (ready, started) = sync_channel(0);
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut result = BurstRead { bytes: Vec::new(), progress: Vec::new() };
        let mut buffer = vec![0u8; 128 * 1024];
        let mut paced = true;
        if ready.send(()).is_err() { return result; }
        while Instant::now() < deadline {
            if paced && !matches!(control.recv_timeout(Duration::from_millis(200)), Err(RecvTimeoutError::Timeout)) {
                paced = false;
            }
            match client.read(&mut buffer) {
                Ok(0) => return result,
                Ok(count) => {
                    result.bytes.extend_from_slice(&buffer[..count]);
                    if paced { result.progress.push(Instant::now()); }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    if !matches!(control.try_recv(), Err(TryRecvError::Empty)) { paced = false; }
                }
                Err(_) => return result,
            }
        }
        result
    });
    started.recv().unwrap();
    (stop, worker)
}

fn assert_partial(result: &io::Result<()>, elapsed: Duration, received: &BurstRead, expected: &[u8],
                  deadline: Instant, buffers: (i32, i32)) {
    let progress = received.progress.iter().filter(|time| **time < deadline).count();
    eprintln!("writer={result:?} elapsed={elapsed:?} bytes={}/{} paced_reads_before_deadline={progress} buffers={buffers:?}",
        received.bytes.len(), expected.len());
    assert_eq!(result.as_ref().err().map(io::Error::kind), Some(io::ErrorKind::TimedOut));
    assert!(elapsed < Duration::from_secs(4), "elapsed={elapsed:?}");
    assert!(received.bytes.len() > 0 && received.bytes.len() < expected.len());
    assert!(received.bytes == expected[..received.bytes.len()], "response prefix was corrupted");
    assert!(progress >= 2, "reader did not make repeated progress before the deadline");
}

#[test]
fn response_deadline_limits_a_progressing_reader() {
    let body = format!("{{\"pad\":\"{}\"}}", "0123456789abcdef".repeat(256 * 1024));
    let expected = format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let (mut server, client) = streams();
    let buffers = (small_buffer(&server, true), small_buffer(&client, false));
    let (stop, reader) = burst_reader(client);
    let budget = Duration::from_secs(2);
    let start = Instant::now();
    let result = write_response(&mut server, 200, &[], &body, budget);
    let elapsed = start.elapsed();
    let _ = stop.send(());
    drop(server);
    let received = reader.join().unwrap();
    assert_partial(&result, elapsed, &received, expected.as_bytes(), start + budget, buffers);
}

#[test]
fn tracked_response_does_not_acknowledge_a_progressing_reader() {
    let request = json!({"jsonrpc":"2.0", "id":"deadline", "method":"tools/call"});
    let message = json!({"jsonrpc":"2.0", "id":"deadline", "result":{
        "isError":false, "pad":"0123456789abcdef".repeat(256 * 1024),
    }});
    let body = message.to_string();
    let expected = format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let (mut server, client) = streams();
    let buffers = (small_buffer(&server, true), small_buffer(&client, false));
    let (stop, reader) = burst_reader(client);
    let (sender, receipt) = sync_channel(1);
    let reply = RpcReply::tracked(&request, Some(message), sender);
    let budget = Duration::from_secs(2);
    let start = Instant::now();
    let result = reply.write_http(&mut server, false, budget);
    let elapsed = start.elapsed();
    let _ = stop.send(());
    drop(server);
    let received = reader.join().unwrap();
    let delivered = receipt.recv_timeout(Duration::from_secs(1));
    eprintln!("tracked receipt={delivered:?}");
    assert_partial(&result, elapsed, &received, expected.as_bytes(), start + budget, buffers);
    assert!(matches!(delivered, Ok(Err(_))), "partial response acknowledged admission");
}

#[test]
fn expired_response_sends_nothing_and_keeps_blocking_reads() {
    for empty in [false, true] {
        let (mut server, mut client) = streams();
        server.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let peer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let sent = client.write_all(b"late");
            let _ = client.shutdown(Shutdown::Write);
            let mut bytes = Vec::new();
            let read = client.read_to_end(&mut bytes);
            (sent, read, bytes)
        });
        let result = if empty {
            write_all_before(&mut server, &[], Instant::now())
        } else {
            write_response(&mut server, 200, &[], "{}", Duration::ZERO)
        };
        let mut inbound = [0u8; 4];
        let read = server.read_exact(&mut inbound);
        drop(server);
        let (sent, peer_read, bytes) = peer.join().unwrap();
        assert_eq!(result.as_ref().err().map(io::Error::kind), Some(io::ErrorKind::TimedOut));
        assert!(read.is_ok() && sent.is_ok() && peer_read.is_ok());
        assert_eq!(&inbound, b"late");
        assert!(bytes.is_empty());
    }
}

#[test]
fn successful_response_drains_delayed_input_before_returning() {
    let (mut server, mut client) = streams();
    client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let (sender, returned) = sync_channel(1);
    let writer = thread::spawn(move || {
        let result = write_response(&mut server, 200, &[], "{}", Duration::from_secs(3));
        let _ = sender.send(result);
    });
    let mut bytes = Vec::new();
    let read = client.read_to_end(&mut bytes);
    let early = returned.recv_timeout(Duration::from_millis(50));
    let held = matches!(&early, Err(RecvTimeoutError::Timeout));
    let sent = client.write_all(b"delayed input");
    let shutdown = client.shutdown(Shutdown::Write);
    let result = match early { Ok(result) => Ok(result), Err(_) => returned.recv_timeout(Duration::from_secs(4)) };
    let joined = writer.join();
    assert!(joined.is_ok() && read.is_ok() && sent.is_ok() && shutdown.is_ok());
    assert!(held, "response returned before releasing the peer's write half");
    assert!(result.unwrap().is_ok());
    assert_eq!(bytes, b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}");
}

#[cfg(windows)]
mod portability_probes {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum SendPrimitive {
        NonblockingVectored,
        NonblockingChunked,
        BlockingTimeout,
    }

    fn send_before(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant, primitive: SendPrimitive) -> io::Result<()> {
        let timed_out = || io::Error::new(io::ErrorKind::TimedOut, "Windows primitive probe timed out");
        let mut longest = Duration::ZERO;
        let outcome = (|| {
            stream.set_nonblocking(!matches!(primitive, SendPrimitive::BlockingTimeout))?;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() { return Err(timed_out()); }
                if bytes.is_empty() { break; }
                if matches!(primitive, SendPrimitive::BlockingTimeout) {
                    stream.set_write_timeout(Some(left))?;
                }
                let start = Instant::now();
                let written = match primitive {
                    SendPrimitive::NonblockingVectored => stream.write_vectored(&[io::IoSlice::new(bytes)]),
                    SendPrimitive::NonblockingChunked => stream.write(&bytes[..bytes.len().min(16 * 1024)]),
                    SendPrimitive::BlockingTimeout => stream.write(bytes),
                };
                longest = longest.max(start.elapsed());
                match written {
                    Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "Windows primitive probe stopped")),
                    Ok(count) => bytes = &bytes[count..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() { return Err(timed_out()); }
                        thread::sleep(left.min(Duration::from_millis(1)));
                    }
                    Err(error) => return Err(error),
                }
            }
            stream.flush()?;
            if Instant::now() >= deadline { return Err(timed_out()); }
            Ok(())
        })();
        let restored = stream.set_nonblocking(false);
        eprintln!("WINDOWS_PRIMITIVE_PROBE primitive={primitive:?} longest_send={longest:?} result={outcome:?} restored={restored:?}");
        outcome.and(restored)?;
        if Instant::now() >= deadline { return Err(timed_out()); }
        Ok(())
    }

    fn run(primitive: SendPrimitive) {
        let payload = vec![b'x'; 4 * 1024 * 1024];
        let budget = Duration::from_secs(2);
        let (mut server, client) = streams();
        let buffers = (small_buffer(&server, true), small_buffer(&client, false));
        let (stop, reader) = burst_reader(client);
        let start = Instant::now();
        let result = send_before(&mut server, &payload, start + budget, primitive);
        let _ = stop.send(());
        drop(server);
        let elapsed = start.elapsed();
        let received = reader.join().unwrap();
        let paced = (result, elapsed, received, start + budget);

        let (mut server, client) = streams();
        let _ = (small_buffer(&server, true), small_buffer(&client, false));
        let (ready, started) = sync_channel(0);
        let peer = thread::spawn(move || {
            ready.send(()).unwrap();
            thread::sleep(Duration::from_secs(5));
            drop(client);
        });
        started.recv().unwrap();
        let start = Instant::now();
        let result = send_before(&mut server, &payload, start + budget, primitive);
        drop(server);
        let elapsed = start.elapsed();
        peer.join().unwrap();
        eprintln!("WINDOWS_PRIMITIVE_PROBE stalled primitive={primitive:?} elapsed={elapsed:?} result={result:?}");
        assert_partial(&paced.0, paced.1, &paced.2, &payload, paced.3, buffers);
        assert_eq!(result.as_ref().err().map(io::Error::kind), Some(io::ErrorKind::TimedOut));
        assert!(elapsed < Duration::from_secs(4), "stalled elapsed={elapsed:?}");
    }

    #[test]
    fn windows_vectored_primitive_deadline_probe() { run(SendPrimitive::NonblockingVectored); }

    #[test]
    fn windows_chunked_primitive_deadline_probe() { run(SendPrimitive::NonblockingChunked); }

    #[test]
    fn windows_blocking_timeout_primitive_deadline_probe() { run(SendPrimitive::BlockingTimeout); }
}
