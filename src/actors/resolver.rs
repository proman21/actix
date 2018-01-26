//! DNS resolver and connector utility actor
//!
//! ## Example
//!
//! ```rust
//! # extern crate actix;
//! # extern crate futures;
//! # use futures::{future, Future};
//! use actix::prelude::*;
//! use actix::actors::resolver;
//!
//! fn main() {
//!     let sys = System::new("test");
//!
//!     Arbiter::handle().spawn({
//!         let resolver: LocalAddress<_> = Arbiter::registry().get::<resolver::Connector>();
//!
//!         resolver.call_fut(
//!             resolver::Resolve::host("localhost"))       // <- resolve "localhost"
//!                 .then(|addrs| {
//!                     println!("RESULT: {:?}", addrs);
//! #                   Arbiter::system().send(actix::msgs::SystemExit(0));
//!                     Ok::<_, ()>(())
//!                 })
//!    });
//!
//!     Arbiter::handle().spawn({
//!         let resolver: LocalAddress<_> = Arbiter::registry().get::<resolver::Connector>();
//!
//!         resolver.call_fut(
//!             resolver::Connect::host("localhost:5000"))  // <- connect to a "localhost"
//!                 .then(|stream| {
//!                     println!("RESULT: {:?}", stream);
//!                     Ok::<_, ()>(())
//!                 })
//!    });
//!
//!    sys.run();
//! }
//! ```
use std::io;
use std::net::SocketAddr;
use std::collections::VecDeque;
use std::time::Duration;

use trust_dns_resolver::ResolverFuture;
use trust_dns_resolver::config::{ResolverConfig, ResolverOpts};
use trust_dns_resolver::lookup_ip::LookupIpFuture;
use futures::{Async, Future, Poll};
use tokio_core::reactor::Timeout;
use tokio_core::net::{TcpStream, TcpStreamNew};

use prelude::*;


pub struct Resolve {
    name: String,
    port: Option<u16>,
}

impl Resolve {
    pub fn host<T: AsRef<str>>(host: T) -> Resolve {
        Resolve{name: host.as_ref().to_owned(), port: None}
    }
    pub fn host_and_port<T: AsRef<str>>(host: T, port: u16) -> Resolve {
        Resolve{name: host.as_ref().to_owned(), port: Some(port)}
    }
}

impl ResponseType for Resolve {
    type Item = VecDeque<SocketAddr>;
    type Error = ConnectorError;
}

pub struct Connect {
    name: String,
    port: Option<u16>,
}

impl Connect {
    pub fn host<T: AsRef<str>>(host: T) -> Connect {
        Connect{name: host.as_ref().to_owned(), port: None}
    }
    pub fn host_and_port<T: AsRef<str>>(host: T, port: u16) -> Connect {
        Connect{name: host.as_ref().to_owned(), port: Some(port)}
    }
}

impl ResponseType for Connect {
    type Item = TcpStream;
    type Error = ConnectorError;
}

#[derive(Fail, Debug)]
pub enum ConnectorError {
    /// Failed to resolve the hostname
    #[fail(display = "Failed resolving hostname: {}", _0)]
    Resolver(String),

    /// Address is invalid
    #[fail(display = "Invalid input: {}", _0)]
    InvalidInput(&'static str),

    /// Connecting took too long
    #[fail(display = "Timeout out while establishing connection")]
    Timeout,

    /// Connection io error
    #[fail(display = "{}", _0)]
    IoError(io::Error),
}

pub struct Connector {
    resolver: ResolverFuture,
}

impl Actor for Connector {
    type Context = Context<Self>;
}

impl Supervised for Connector {}

impl actix::ArbiterService for Connector {}

impl Default for Connector {
    fn default() -> Connector {
        let resolver = match ResolverFuture::from_system_conf(Arbiter::handle()) {
            Ok(resolver) => resolver,
            Err(err) => {
                warn!("Can not create system dns resolver: {}", err);
                ResolverFuture::new(
                    ResolverConfig::default(),
                    ResolverOpts::default(),
                    Arbiter::handle())
            }
        };
        Connector{resolver: resolver}
    }
}

impl Handler<Resolve> for Connector {
    type Result = ResponseFuture<Self, Resolve>;

    fn handle(&mut self, msg: Resolve, _: &mut Self::Context) -> Self::Result {
        Box::new(Resolver::new(msg.name, msg.port.unwrap_or(0), &self.resolver))
    }
}

impl Handler<Connect> for Connector {
    type Result = ResponseFuture<Self, Connect>;

    fn handle(&mut self, msg: Connect, _: &mut Self::Context) -> Self::Result {
        Box::new(
            Resolver::new(msg.name, msg.port.unwrap_or(0), &self.resolver)
                .and_then(|addrs, _, _| TcpConnector::new(addrs)))
    }
}

/// Resolver future
struct Resolver {
    lookup: Option<LookupIpFuture>,
    port: u16,
    addrs: Option<VecDeque<SocketAddr>>,
    error: Option<ConnectorError>,
}

impl Resolver {

    pub fn new<S: AsRef<str>>(addr: S, port: u16, resolver: &ResolverFuture) -> Resolver {
        // try to parse as a regular SocketAddr first
        if let Ok(addr) = addr.as_ref().parse() {
            let mut addrs = VecDeque::new();
            addrs.push_back(addr);

            Resolver {
                lookup: None,
                port: port,
                addrs: Some(addrs),
                error: None }
        } else {
            // we need to do dns resolution
            match Resolver::parse(addr.as_ref(), port) {
                Ok((host, port)) => Resolver {
                    lookup: Some(resolver.lookup_ip(host)),
                    port: port,
                    addrs: None,
                    error: None },
                Err(err) => Resolver {
                    lookup: None,
                    port: port,
                    addrs: None,
                    error: Some(err) }
            }
        }
    }

    fn parse(addr: &str, port: u16) -> Result<(&str, u16), ConnectorError> {
        macro_rules! try_opt {
            ($e:expr, $msg:expr) => (
                match $e {
                    Some(r) => r,
                    None => return Err(ConnectorError::InvalidInput($msg)),
                }
            )
        }

        // split the string by ':' and convert the second part to u16
        let mut parts_iter = addr.rsplitn(2, ':');
        let port_str = try_opt!(parts_iter.next(), "invalid socket address");
        let host = try_opt!(parts_iter.next(), "invalid socket address");
        let port: u16 = port_str.parse().unwrap_or(port);

        Ok((host, port))
    }
}

impl ActorFuture for Resolver {
    type Item = VecDeque<SocketAddr>;
    type Error = ConnectorError;
    type Actor = Connector;

    fn poll(&mut self, _: &mut Connector, _: &mut Context<Connector>)
            -> Poll<Self::Item, Self::Error>
    {
        if let Some(err) = self.error.take() {
            Err(err)
        } else if let Some(addrs) = self.addrs.take() {
            Ok(Async::Ready(addrs))
        } else {
            match self.lookup.as_mut().unwrap().poll() {
                Ok(Async::NotReady) => Ok(Async::NotReady),
                Ok(Async::Ready(ips)) => {
                    let addrs: VecDeque<_> =
                        ips.iter().map(|ip| SocketAddr::new(ip, self.port)).collect();
                    if addrs.is_empty() {
                        Err(ConnectorError::Resolver(
                            "Expect at least one A dns record".to_owned()))
                    } else {
                        Ok(Async::Ready(addrs))
                    }
                },
                Err(err) => Err(ConnectorError::Resolver(format!("{}", err))),
            }
        }
    }
}

/// Tcp stream connector
pub struct TcpConnector {
    addrs: VecDeque<SocketAddr>,
    timeout: Timeout,
    stream: Option<TcpStreamNew>,
}

impl TcpConnector {

    pub fn new(addrs: VecDeque<SocketAddr>) -> TcpConnector {
        TcpConnector::with_timeout(addrs, Duration::from_secs(1))
    }

    pub fn with_timeout(addrs: VecDeque<SocketAddr>, timeout: Duration) -> TcpConnector {
        TcpConnector {
            addrs: addrs,
            stream: None,
            timeout: Timeout::new(timeout, Arbiter::handle()).unwrap() }
    }
}

impl ActorFuture for TcpConnector {
    type Item = TcpStream;
    type Error = ConnectorError;
    type Actor = Connector;

    fn poll(&mut self, _: &mut Connector, _: &mut Context<Connector>)
            -> Poll<Self::Item, Self::Error>
    {
        // timeout
        if let Ok(Async::Ready(_)) = self.timeout.poll() {
            return Err(ConnectorError::Timeout)
        }

        // connect
        loop {
            if let Some(new) = self.stream.as_mut() {
                match new.poll() {
                    Ok(Async::Ready(sock)) => return Ok(Async::Ready(sock)),
                    Ok(Async::NotReady) => return Ok(Async::NotReady),
                    Err(err) => {
                        if self.addrs.is_empty() {
                            return Err(ConnectorError::IoError(err))
                        }
                    }
                }
            }

            // try to connect
            let addr = self.addrs.pop_front().unwrap();
            self.stream = Some(TcpStream::connect(&addr, Arbiter::handle()));
        }
    }
}
