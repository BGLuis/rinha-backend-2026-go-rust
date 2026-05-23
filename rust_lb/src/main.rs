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

    let mut ring = io_uring::IoUring::new(1024)?;

    let mut rr = 0;

    let mut sq = ring.submission();
    for _ in 0..128 {
        let accept_e = io_uring::opcode::Accept::new(io_uring::types::Fd(listen_fd), ptr::null_mut::<libc::sockaddr>(), ptr::null_mut::<libc::socklen_t>())
            .flags(libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC)
            .build()
            .user_data(1);
        unsafe { sq.push(&accept_e).unwrap(); }
    }
    drop(sq);
    ring.submit()?;

    loop {
        ring.submit_and_wait(1)?;
        let mut cq = ring.completion();
        cq.sync();
        
        let mut accepted_count = 0;
        let mut batches: Vec<Vec<libc::c_int>> = vec![Vec::with_capacity(16); up_addrs.len()];

        for cqe in cq {
            if cqe.user_data() == 1 {
                let res = cqe.result();
                if res >= 0 {
                    let client_fd = res;
                    unsafe {
                        let one: libc::c_int = 1;
                        libc::setsockopt(client_fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, &one as *const _ as *const libc::c_void, mem::size_of::<libc::c_int>() as libc::socklen_t);
                    }

                    let target_idx = rr % up_addrs.len();
                    batches[target_idx].push(client_fd);
                    rr = rr.wrapping_add(1);
                }
                accepted_count += 1;
            }
        }

        for (i, batch) in batches.iter().enumerate() {
            if !batch.is_empty() {
                for chunk in batch.chunks(16) {
                    send_fds(uds_fd, &up_addrs[i], chunk);
                }
                for &fd in batch { unsafe { libc::close(fd); } }
            }
        }
        
        if accepted_count > 0 {
            let mut sq = ring.submission();
            for _ in 0..accepted_count {
                let accept_e = io_uring::opcode::Accept::new(io_uring::types::Fd(listen_fd), ptr::null_mut::<libc::sockaddr>(), ptr::null_mut::<libc::socklen_t>())
                    .flags(libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC)
                    .build()
                    .user_data(1);
                unsafe { sq.push(&accept_e).unwrap_or(()); }
            }
        }
    }
    
    Ok(())
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
