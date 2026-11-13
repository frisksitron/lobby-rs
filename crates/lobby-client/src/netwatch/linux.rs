#![allow(unsafe_code)]

use std::{
    io::{self, PipeReader, PipeWriter, Write},
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    thread::{self, JoinHandle},
};

use tracing::{debug, warn};

/// Room for a burst of messages: one `recv` takes whole messages, and a larger burst than this
/// is read by the next `recv`.
const RECV_BUFFER: usize = 16 * 1024;

const NLMSG_HEADER: usize = size_of::<libc::nlmsghdr>();

/// While it lives, `on_change` runs after every address or link added, removed or changed, on a
/// thread of its own, so it must not block. Dropping it wakes the thread and joins it.
pub(crate) struct NetworkWatch {
    wake: Option<PipeWriter>,
    thread: Option<JoinHandle<()>>,
}

impl NetworkWatch {
    pub(crate) fn start(on_change: impl Fn() + Send + Sync + 'static) -> Option<NetworkWatch> {
        let socket = open_route_socket()
            .inspect_err(|error| warn!(%error, "network changes not watched"))
            .ok()?;
        let (wake_rx, wake) = io::pipe()
            .inspect_err(|error| warn!(%error, "network changes not watched"))
            .ok()?;
        let thread = thread::Builder::new()
            .name("netwatch".into())
            .spawn(move || watch(&socket, &wake_rx, on_change))
            .inspect_err(|error| warn!(%error, "network changes not watched"))
            .ok()?;
        Some(NetworkWatch {
            wake: Some(wake),
            thread: Some(thread),
        })
    }
}

impl Drop for NetworkWatch {
    fn drop(&mut self) {
        if let Some(mut wake) = self.wake.take() {
            // A thread that already ended dropped the read end: the write fails and there is
            // nothing to wake.
            let _ = wake.write_all(&[0]);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A rtnetlink socket subscribed to link and address changes, non-blocking.
fn open_route_socket() -> io::Result<OwnedFd> {
    // SAFETY: a plain syscall on constants.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a new descriptor that nothing else owns.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: sockaddr_nl is plain integers, all zero is a valid value.
    let mut addr: libc::sockaddr_nl = unsafe { zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    addr.nl_groups =
        (libc::RTMGRP_LINK | libc::RTMGRP_IPV4_IFADDR | libc::RTMGRP_IPV6_IFADDR) as u32;
    // SAFETY: `addr` is a live sockaddr_nl and its size is passed with it.
    let bound = unsafe {
        libc::bind(
            socket.as_raw_fd(),
            (&raw const addr).cast(),
            size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if bound < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

fn watch(socket: &OwnedFd, wake: &PipeReader, on_change: impl Fn()) {
    let mut fds = [
        poll_for_input(socket.as_raw_fd()),
        poll_for_input(wake.as_raw_fd()),
    ];
    let mut buffer = vec![0u8; RECV_BUFFER];
    loop {
        // SAFETY: `fds` is a live array of the length passed.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            warn!(%error, "network watch stopped");
            return;
        }
        if fds[1].revents != 0 {
            return;
        }
        if fds[0].revents & libc::POLLIN == 0 {
            warn!(revents = fds[0].revents, "network watch stopped");
            return;
        }
        match drain(socket.as_raw_fd(), &mut buffer) {
            Ok(true) => {
                debug!("a link or an address changed");
                on_change();
            }
            Ok(false) => {}
            Err(error) => {
                warn!(%error, "network watch stopped");
                return;
            }
        }
    }
}

fn poll_for_input(fd: RawFd) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

/// Reads everything waiting; true when any of it is an address or a link changing.
fn drain(fd: RawFd, buffer: &mut [u8]) -> io::Result<bool> {
    let mut changed = false;
    loop {
        // SAFETY: `buffer` is a live buffer of the length passed.
        let read = unsafe { libc::recv(fd, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
        if read < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => return Ok(changed),
                io::ErrorKind::Interrupted => continue,
                // The kernel dropped messages because they were not read in time: something
                // changed, and which thing is lost.
                _ if error.raw_os_error() == Some(libc::ENOBUFS) => changed = true,
                _ => return Err(error),
            }
            continue;
        }
        if read == 0 {
            return Ok(changed);
        }
        changed |= has_network_change(&buffer[..read as usize]);
    }
}

/// Whether the netlink messages in `bytes` include an address or a link added, removed or
/// changed. Only each message's header is read. A cable pulled and put back, or Wi-Fi dropping
/// and joining, is link messages and no address change when the address is static; a link
/// message does not come for traffic counters, only for flags, carrier, state and the like.
fn has_network_change(mut bytes: &[u8]) -> bool {
    while bytes.len() >= NLMSG_HEADER {
        let length = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let kind = u16::from_ne_bytes([bytes[4], bytes[5]]);
        if matches!(
            kind,
            libc::RTM_NEWADDR | libc::RTM_DELADDR | libc::RTM_NEWLINK | libc::RTM_DELLINK
        ) {
            return true;
        }
        if length < NLMSG_HEADER || length > bytes.len() {
            return false;
        }
        // Messages sit on 4-byte boundaries.
        let step = length.next_multiple_of(4).min(bytes.len());
        bytes = &bytes[step..];
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    /// The thread starts, and dropping the watch joins it: the drop returns only once the
    /// thread has ended.
    #[test]
    fn dropping_the_watch_ends_its_thread() {
        let watch = NetworkWatch::start(|| {}).expect("a netlink socket opens");
        drop(watch);
    }

    /// How long a change gets to reach the watch. Only a failing test waits it out.
    const HEARS_WITHIN: Duration = Duration::from_secs(5);

    fn ip(args: &[&str]) {
        let status = std::process::Command::new("ip")
            .args(args)
            .status()
            .expect("ip runs");
        assert!(
            status.success(),
            "ip {args:?} failed: run this in `unshare -n`"
        );
    }

    /// Starts a watch of its own, so what an earlier change sent cannot be heard as this one,
    /// runs `ip args`, and says whether the watch fired. Dropping it joins the thread.
    fn fires_for(args: &[&str]) -> bool {
        let (fired, heard) = mpsc::channel();
        let watch = NetworkWatch::start(move || {
            let _ = fired.send(());
        })
        .expect("a netlink socket opens");
        ip(args);
        let fired = heard.recv_timeout(HEARS_WITHIN).is_ok();
        drop(watch);
        assert!(
            heard.recv().is_err(),
            "the thread is gone, so its sender is dropped"
        );
        fired
    }

    /// The real kernel, in a network namespace of its own, because changing an interface needs
    /// root and must not touch the machine's:
    /// `sudo unshare -n sh -c 'ip link set lo up; <this test binary> --ignored --exact <name>'`.
    /// An address added and a link changing each fire the watch. The link is a dummy interface
    /// with no address, so its changes are link messages and nothing else, as a cable pulled and
    /// put back with a static address is. (`lo` would not do: taking it down drops its addresses.)
    #[test]
    #[ignore = "changes an interface: run inside `sudo unshare -n`"]
    fn an_address_or_a_link_changing_fires_the_watch() {
        assert!(fires_for(&["addr", "add", "127.0.0.2/8", "dev", "lo"]));
        assert!(fires_for(&["link", "add", "d0", "type", "dummy"]));
        assert!(fires_for(&["link", "set", "d0", "mtu", "1400"]));
        assert!(fires_for(&["link", "del", "d0"]));
    }
}
