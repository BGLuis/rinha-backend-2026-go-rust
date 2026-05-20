use std::ffi::CString;
use std::mem;
use std::os::unix::io::RawFd;
use std::ptr;

const DEFAULT_PORT: u16 = 9999;
const DEFAULT_BACKLOG: i32 = 8192;

fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_PORT);
    let upstreams_env = std::env::var("UPSTREAMS").unwrap_or_else(|_| "".to_string());
    let upstreams: Vec<String> = upstreams_env.split(',').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();

    let listen_fd = create_listener(port, DEFAULT_BACKLOG)?;
    unsafe {
        libc::fcntl(listen_fd, libc::F_SETFL, libc::O_NONBLOCK);
    }

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

    let epfd = unsafe { libc::epoll_create1(0) };
    if epfd < 0 { return Err(std::io::Error::last_os_error()); }

    let mut event = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: listen_fd as u64,
    };
    if unsafe { libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, listen_fd, &mut event) } < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; 4096];
    let mut rr = 0;

    loop {
        let n = unsafe { libc::epoll_wait(epfd, events.as_mut_ptr(), events.capacity() as i32, -1) };
        if n < 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EINTR { continue; }
            break;
        }

        for i in 0..n as usize {
            if events[i].u64 == listen_fd as u64 {
                loop {
                    let client_fd = unsafe { libc::accept4(listen_fd, ptr::null_mut(), ptr::null_mut(), libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) };
                    if client_fd < 0 {
                        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                        if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
                            break;
                        }
                        continue;
                    }

                    unsafe {
                        let one: libc::c_int = 1;
                        libc::setsockopt(client_fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, &one as *const _ as *const libc::c_void, mem::size_of::<libc::c_int>() as libc::socklen_t);
                    }

                    let target_addr = &up_addrs[rr % up_addrs.len()];
                    rr = rr.wrapping_add(1);

                    send_fd(uds_fd, target_addr, client_fd);
                    unsafe { libc::close(client_fd); }
                }
            }
        }
    }
    
    Ok(())
}

fn send_fd(sock: libc::c_int, addr: &libc::sockaddr_un, fd_to_send: libc::c_int) {
    unsafe {
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_name = addr as *const _ as *mut libc::c_void;
        msg.msg_namelen = mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;

        let mut iov = libc::iovec { iov_base: "1".as_ptr() as *mut libc::c_void, iov_len: 1 };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;

        let mut cmsg_buf = [0u8; 24];
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

        libc::sendmsg(sock, &msg, 0);
    }
}

fn create_listener(port: u16, backlog: i32) -> std::io::Result<RawFd> {
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
    }
    Ok(fd)
}
