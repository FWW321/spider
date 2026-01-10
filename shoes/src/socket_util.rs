use std::mem::ManuallyDrop;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

pub fn new_udp_socket(
    is_ipv6: bool,
    bind_interface: Option<String>,
) -> std::io::Result<tokio::net::UdpSocket> {
    let socket = new_socket2_udp_socket(
        is_ipv6,
        bind_interface,
        Some(get_unspecified_socket_addr(is_ipv6)),
        false,
    )?;

    into_tokio_udp_socket(socket)
}

fn get_unspecified_socket_addr(is_ipv6: bool) -> SocketAddr {
    if !is_ipv6 {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0)
    } else {
        "[::]:0".parse().unwrap()
    }
}

pub fn new_socket2_udp_socket(
    is_ipv6: bool,
    bind_interface: Option<String>,
    bind_address: Option<SocketAddr>,
    reuse_port: bool,
) -> std::io::Result<socket2::Socket> {
    let domain = if is_ipv6 { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    socket.set_nonblocking(true)?;

    if reuse_port {
        #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
        socket.set_reuse_port(true)?;
    }

    if let Some(ref _interface) = bind_interface {
        #[cfg(target_os = "linux")]
        socket.bind_device(Some(_interface.as_bytes()))?;
    }

    if let Some(bind_address) = bind_address {
        socket.bind(&SockAddr::from(bind_address))?;
    }

    Ok(socket)
}

pub fn new_socket2_udp_socket_with_buffer_size(
    is_ipv6: bool,
    bind_interface: Option<String>,
    bind_address: Option<SocketAddr>,
    reuse_port: bool,
    buffer_size: Option<usize>,
) -> std::io::Result<socket2::Socket> {
    let domain = if is_ipv6 { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    socket.set_nonblocking(true)?;

    if let Some(size) = buffer_size {
        let _ = socket.set_recv_buffer_size(size);
        let _ = socket.set_send_buffer_size(size);
    }

    if reuse_port {
        #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
        socket.set_reuse_port(true)?;
    }

    if let Some(ref _interface) = bind_interface {
        #[cfg(target_os = "linux")]
        socket.bind_device(Some(_interface.as_bytes()))?;
    }

    if let Some(bind_address) = bind_address {
        socket.bind(&SockAddr::from(bind_address))?;
    }

    Ok(socket)
}

fn into_tokio_udp_socket(socket: socket2::Socket) -> std::io::Result<tokio::net::UdpSocket> {
    #[cfg(unix)]
    let std_socket = unsafe { std::net::UdpSocket::from_raw_fd(socket.into_raw_fd()) };
    #[cfg(windows)]
    let std_socket = unsafe { std::net::UdpSocket::from_raw_socket(socket.into_raw_socket()) };

    tokio::net::UdpSocket::from_std(std_socket)
}

pub fn new_tcp_socket(
    bind_interface: Option<String>,
    is_ipv6: bool,
) -> std::io::Result<tokio::net::TcpSocket> {
    let tcp_socket = if is_ipv6 {
        tokio::net::TcpSocket::new_v6()?
    } else {
        tokio::net::TcpSocket::new_v4()?
    };

    if let Some(_interface) = bind_interface {
        #[cfg(target_os = "linux")]
        tcp_socket.bind_device(Some(_interface.as_bytes()))?;
    }

    Ok(tcp_socket)
}

pub fn set_tcp_keepalive(
    tcp_stream: &tokio::net::TcpStream,
    idle_time: std::time::Duration,
    send_interval: std::time::Duration,
) -> std::io::Result<()> {
    #[cfg(unix)]
    let socket = ManuallyDrop::new(unsafe { Socket::from_raw_fd(tcp_stream.as_raw_fd()) });
    #[cfg(windows)]
    let socket = ManuallyDrop::new(unsafe { Socket::from_raw_socket(tcp_stream.as_raw_socket()) });

    if idle_time.is_zero() && send_interval.is_zero() {
        socket.set_keepalive(false)?;
    } else {
        let keepalive = socket2::TcpKeepalive::new()
            .with_time(idle_time)
            .with_interval(send_interval);
        socket.set_keepalive(true)?;
        socket.set_tcp_keepalive(&keepalive)?;
    }
    Ok(())
}

pub fn new_tcp_listener(
    bind_address: SocketAddr,
    backlog: u32,
    bind_interface: Option<String>,
) -> std::io::Result<tokio::net::TcpListener> {
    let domain = if bind_address.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

    socket.set_nonblocking(true)?;
    socket.set_reuse_address(true)?;

    if let Some(ref _interface) = bind_interface {
        #[cfg(target_os = "linux")]
        socket.bind_device(Some(_interface.as_bytes()))?;
    }

    socket.bind(&SockAddr::from(bind_address))?;
    socket.listen(backlog as i32)?;

    let std_listener: std::net::TcpListener = socket.into();
    tokio::net::TcpListener::from_std(std_listener)
}

#[cfg(unix)]
pub fn new_unix_listener<P: AsRef<Path>>(
    path: P,
    backlog: u32,
) -> std::io::Result<tokio::net::UnixListener> {
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.set_nonblocking(true)?;

    let addr = SockAddr::unix(path)?;
    socket.bind(&addr)?;
    socket.listen(backlog as i32)?;

    let std_listener: std::os::unix::net::UnixListener = socket.into();
    tokio::net::UnixListener::from_std(std_listener)
}