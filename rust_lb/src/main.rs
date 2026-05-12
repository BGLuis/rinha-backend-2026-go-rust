use std::ffi::CString;
use std::mem;
use std::os::unix::io::RawFd;
use std::ptr;
use std::thread;
use io_uring::{IoUring, opcode, types};

const DEFAULT_PORT: u16 = 9999;
const DEFAULT_BACKLOG: i32 = 8192;
const DEFAULT_RING_QD: u32 = 8192;
const CQE_F_MORE: u32 = 1 << 1;

pub fn run() -> std::io::Result<()> {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let port = env_u32("PORT", DEFAULT_PORT as u32) as u16;
    let backlog = env_u32("BACKLOG", DEFAULT_BACKLOG as u32) as i32;
    let ring_qd = env_u32("RING_QD", DEFAULT_RING_QD);

    let upstream_str = std::env::var("UPSTREAMS")
        .unwrap_or_else(|_| "/tmp/sockets/api.sock".to_string());

    let upstreams: Vec<String> = upstream_str.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if upstreams.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "No upstreams provided"));
    }

    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    eprintln!("lb listen=:{} backlog={} upstreams={:?} cores={}", port, backlog, upstreams, cores);

    let mut handles = Vec::new();

    for _ in 0..cores {
        let ups = upstreams.clone();
        handles.push(thread::spawn(move || {
            worker_loop(port, backlog, ring_qd, &ups).unwrap();
        }));
    }

    for h in handles {
        let _ = h.join();
    }

    Ok(())
}

fn worker_loop(port: u16, backlog: i32, ring_qd: u32, upstreams: &[String]) -> std::io::Result<()> {
    let listen_fd = create_listener(port, backlog)?;
    let mut ring: IoUring = IoUring::builder()
        .build(ring_qd)?;

    let uds_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0) };
    if uds_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Increase send buffer to avoid dropped FDs
    let sndbuf: libc::c_int = 16 * 1024 * 1024;
    unsafe {
        libc::setsockopt(
            uds_fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &sndbuf as *const _ as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }

    let mut up_addrs = Vec::with_capacity(upstreams.len());
    for ups in upstreams {
        let mut c_path = ups.clone();
        if c_path.len() >= 108 {
            c_path.truncate(107);
        }
        let c_path = CString::new(c_path).unwrap();
        let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        unsafe {
            ptr::copy_nonoverlapping(
                c_path.as_ptr(),
                addr.sun_path.as_mut_ptr() as *mut libc::c_char,
                c_path.as_bytes().len(),
            );
        }
        up_addrs.push(addr);
    }

    push_accept(&mut ring, listen_fd);

    let mut cqes = Vec::with_capacity(ring_qd as usize);
    let mut rr = 0;

    loop {
        ring.submit_and_wait(1)?;

        cqes.clear();
        cqes.extend(ring.completion());
        for cqe in cqes.drain(..) {
            let res = cqe.result();
            let flags = cqe.flags();

            if (flags & CQE_F_MORE) == 0 {
                push_accept(&mut ring, listen_fd);
            }
            if res < 0 {
                continue;
            }
            let client_fd = res as RawFd;
            set_tcp_nodelay(client_fd);

            let target_addr = &up_addrs[rr % up_addrs.len()];
            rr = rr.wrapping_add(1);

            send_fd(uds_fd, target_addr, client_fd);

            unsafe { libc::close(client_fd); }
        }
    }
}

fn send_fd(sock: libc::c_int, addr: &libc::sockaddr_un, fd_to_send: libc::c_int) {
    unsafe {
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_name = addr as *const _ as *mut libc::c_void;
        msg.msg_namelen = mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;

        let mut iov = libc::iovec {
            iov_base: "1".as_ptr() as *mut libc::c_void,
            iov_len: 1,
        };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;

        let mut cmsg_buf = [0u8; 24]; // CMSG_SPACE(sizeof(int))
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = 24;

        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if !cmsg.is_null() {
            (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<libc::c_int>() as u32) as _;
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            let data = libc::CMSG_DATA(cmsg) as *mut libc::c_int;
            *data = fd_to_send;
            msg.msg_controllen = (*cmsg).cmsg_len;
        }

        let mut retries = 0;
        loop {
            let res = libc::sendmsg(sock, &msg, 0);
            if res >= 0 {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                retries += 1;
                if retries > 100 {
                    break;
                }
                std::thread::yield_now();
            } else {
                eprintln!("sendmsg failed: {} (sock={}, fd_to_send={})", err, sock, fd_to_send);
                break;
            }
        }
    }
}

fn push_accept(ring: &mut IoUring, listen_fd: RawFd) {
    let sqe = opcode::AcceptMulti::new(types::Fd(listen_fd))
        .build()
        .user_data(1);
    loop {
        unsafe {
            if ring.submission().push(&sqe).is_ok() {
                return;
            }
        }
        let _ = ring.submit();
    }
}

fn create_listener(port: u16, backlog: i32) -> std::io::Result<RawFd> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &one as *const _ as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &one as *const _ as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }

    let mut addr: libc::sockaddr_in = unsafe { mem::zeroed() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = port.to_be();
    addr.sin_addr.s_addr = libc::INADDR_ANY.to_be();
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd); }
        return Err(err);
    }
    let rc = unsafe { libc::listen(fd, backlog) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd); }
        return Err(err);
    }
    Ok(fd)
}

fn set_tcp_nodelay(fd: RawFd) {
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            &one as *const _ as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("lb requires Linux");
    std::process::exit(1);
}
