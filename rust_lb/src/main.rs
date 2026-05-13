use std::ffi::CString;
use std::mem;
use std::os::unix::io::RawFd;
use std::ptr;
use io_uring::{IoUring, opcode, types};

const DEFAULT_PORT: u16 = 9999;
const DEFAULT_BACKLOG: i32 = 8192;
const DEFAULT_RING_QD: u32 = 4096;

fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_PORT);
    let upstreams_env = std::env::var("UPSTREAMS").unwrap_or_else(|_| "".to_string());
    let upstreams: Vec<String> = upstreams_env.split(',').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    
    let listen_fd = create_listener(port, DEFAULT_BACKLOG)?;
    
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

    let mut ring = IoUring::builder().build(DEFAULT_RING_QD)?;
    push_accept(&mut ring, listen_fd);

    let mut cqes = Vec::with_capacity(DEFAULT_RING_QD as usize);
    let mut rr = 0;

    loop {
        ring.submit_and_wait(1)?;
        cqes.clear();
        cqes.extend(ring.completion());
        for cqe in cqes.drain(..) {
            let res = cqe.result();
            if (cqe.flags() & 1 << 0) == 0 { // CQE_F_MORE
                push_accept(&mut ring, listen_fd);
            }
            if res < 0 { continue; }
            
            let client_fd = res as RawFd;
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

        // Blocking send with SEQPACKET provides backpressure
        libc::sendmsg(sock, &msg, 0);
    }
}

fn push_accept(ring: &mut IoUring, listen_fd: RawFd) {
    let sqe = opcode::AcceptMulti::new(types::Fd(listen_fd)).build().user_data(1);
    unsafe { let _ = ring.submission().push(&sqe); }
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
