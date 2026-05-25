use std::ffi::CString;
use std::mem;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::ptr;
use std::io::ErrorKind;

use mio::net::TcpListener;
use mio::{Events, Interest, Poll, Token};

const DEFAULT_PORT: u16 = 9999;
const SERVER_TOKEN: Token = Token(0);

fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_PORT);
    let upstreams_env = std::env::var("UPSTREAMS").unwrap_or_else(|_| "".to_string());
    let upstreams: Vec<String> = upstreams_env.split(',').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();

    // UDS socket MUST be BLOCKING to provide backpressure and avoid dropping FDs
    let uds_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0) };
    if uds_fd < 0 { return Err(std::io::Error::last_os_error()); }

    let sndbuf: libc::c_int = 16 * 1024 * 1024;
    unsafe {
        libc::setsockopt(uds_fd, libc::SOL_SOCKET, libc::SO_SNDBUF, &sndbuf as *const _ as *const libc::c_void, mem::size_of::<libc::c_int>() as libc::socklen_t);
    }

    let mut up_addrs = Vec::with_capacity(upstreams.len());
    for ups in upstreams {
        let c_path = CString::new(ups).unwrap();
        let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let path_bytes = c_path.as_bytes_with_nul();
        unsafe {
            ptr::copy_nonoverlapping(path_bytes.as_ptr(), addr.sun_path.as_mut_ptr() as *mut u8, std::cmp::min(path_bytes.len(), 108));
        }
        up_addrs.push(addr);
    }

    let std_listener = create_listener(port, 8192)?;
    let mut listener = TcpListener::from_std(std_listener);

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(4096);

    poll.registry().register(&mut listener, SERVER_TOKEN, Interest::READABLE)?;

    let mut rr = 0;
    let mut batches: Vec<Vec<libc::c_int>> = vec![Vec::with_capacity(16); up_addrs.len()];

    loop {
        if let Err(e) = poll.poll(&mut events, None) {
            if e.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }

        for event in events.iter() {
            if event.token() == SERVER_TOKEN {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(_) = stream.set_nodelay(true) {
                                continue;
                            }
                            
                            let raw_fd = stream.into_raw_fd();
                            let target_idx = rr % up_addrs.len();
                            batches[target_idx].push(raw_fd);
                            rr = rr.wrapping_add(1);
                        }
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                            break;
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => {
                            continue;
                        }
                        Err(_) => {
                            break;
                        }
                    }
                }
            }
        }

        for (i, batch) in batches.iter_mut().enumerate() {
            if !batch.is_empty() {
                for chunk in batch.chunks(16) {
                    send_fds(uds_fd, &up_addrs[i], chunk);
                }
                for &fd in batch.iter() { 
                    unsafe { libc::close(fd); } 
                }
                batch.clear();
            }
        }
    }
}

fn send_fds(sock: libc::c_int, addr: &libc::sockaddr_un, fds: &[libc::c_int]) {
    unsafe {
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_name = addr as *const _ as *mut libc::c_void;
        msg.msg_namelen = mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;

        let mut iov = libc::iovec { iov_base: "1".as_ptr() as *mut libc::c_void, iov_len: 1 };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;

        let mut cmsg_buf = [0u8; 128];
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_buf.len() as _;

        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if !cmsg.is_null() {
            let fd_size = (fds.len() * mem::size_of::<libc::c_int>()) as u32;
            (*cmsg).cmsg_len = libc::CMSG_LEN(fd_size) as _;
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            let data = libc::CMSG_DATA(cmsg) as *mut libc::c_int;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), data, fds.len());
            msg.msg_controllen = (*cmsg).cmsg_len;
        }

        libc::sendmsg(sock, &msg, 0);
    }
}

fn create_listener(port: u16, backlog: i32) -> std::io::Result<std::net::TcpListener> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 { return Err(std::io::Error::last_os_error()); }
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one as *const _ as *const libc::c_void, mem::size_of::<libc::c_int>() as libc::socklen_t);
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, &one as *const _ as *const libc::c_void, mem::size_of::<libc::c_int>() as libc::socklen_t);
        let mut addr: libc::sockaddr_in = mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = port.to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY.to_be();
        if libc::bind(fd, &addr as *const _ as *const libc::sockaddr, mem::size_of::<libc::sockaddr_in>() as libc::socklen_t) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::listen(fd, backlog) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        
        let std_listener = std::net::TcpListener::from_raw_fd(fd);
        std_listener.set_nonblocking(true)?;
        Ok(std_listener)
    }
}
