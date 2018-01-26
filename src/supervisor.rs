use std::mem;
use futures::{Future, Async, Poll, Stream};

use actor::{Actor, Supervised, ActorContext, AsyncContext};
use arbiter::Arbiter;
use address::{Address, LocalAddress};
use context::Context;
use msgs::Execute;
use local::{LocalAddrReceiver, LocalAddrProtocol};
use addr::channel as sync;
use addr::AddressReceiver;

/// Actor supervisor
///
/// Supervisor manages incoming message for actor. In case of actor failure, supervisor
/// creates new execution context and restarts actor lifecycle. Supervisor does not
/// does not re-create actor, it just calls `restarting()` method.
///
/// Supervisor has same livecycle as actor. In situation when all addresses to supervisor
/// get dropped and actor does not execute anything, supervisor terminates.
///
/// `Supervisor` can not guarantee that actor successfully process incoming message.
/// If actor fails during message processing, this message can not be recovered. Sender
/// would receive `Err(Cancelled)` error in this situation.
///
/// ## Example
///
/// ```rust
/// # #[macro_use] extern crate actix;
/// # use actix::prelude::*;
/// #[derive(Message)]
/// struct Die;
///
/// struct MyActor;
///
/// impl Actor for MyActor {
///     type Context = Context<Self>;
/// }
///
/// // To use actor with supervisor actor has to implement `Supervised` trait
/// impl actix::Supervised for MyActor {
///     fn restarting(&mut self, ctx: &mut Context<MyActor>) {
///         println!("restarting");
///     }
/// }
///
/// impl Handler<Die> for MyActor {
///     type Result = ();
///
///     fn handle(&mut self, _: Die, ctx: &mut Context<MyActor>) {
///         ctx.stop();
/// #       Arbiter::system().send(actix::msgs::SystemExit(0));
///     }
/// }
///
/// fn main() {
///     let sys = System::new("test");
///
///     let addr = actix::Supervisor::start(|_| MyActor);
///
///     addr.send(Die);
///     sys.run();
/// }
/// ```
pub struct Supervisor<A: Supervised> where A: Actor<Context=Context<A>> {
    ctx: A::Context,
    #[allow(dead_code)]
    addr: LocalAddress<A>,
    sync_msgs: Option<AddressReceiver<A>>,
    unsync_msgs: LocalAddrReceiver<A>,
}

impl<A> Supervisor<A> where A: Supervised + Actor<Context=Context<A>>
{
    /// Start new supervised actor in current Arbiter.
    pub fn start<F>(f: F) -> LocalAddress<A>
        where A: Actor<Context=Context<A>>,
              F: FnOnce(&mut A::Context) -> A + 'static
    {
        // create actor
        let mut ctx = Context::new(None);
        let addr = ctx.address();
        let act = f(&mut ctx);
        ctx.set_actor(act);

        // create supervisor
        let rx = LocalAddrReceiver::new(0);
        let mut supervisor = Supervisor {
            ctx: ctx,
            addr: addr,
            sync_msgs: None,
            unsync_msgs: rx };

        let addr = supervisor.unsync_msgs.sender();
        Arbiter::handle().spawn(supervisor);

        LocalAddress::new(addr)
    }

    /// Start new supervised actor in arbiter's thread. Depends on `lazy` argument
    /// actor could be started immediately or on first incoming message.
    pub fn start_in<F>(addr: &Address<Arbiter>, f: F) -> Address<A>
        where A: Actor<Context=Context<A>>,
              F: FnOnce(&mut Context<A>) -> A + Send + 'static
    {
        let (tx, rx) = sync::channel(0);

        addr.send(Execute::new(move || -> Result<(), ()> {
            // create actor
            let mut ctx = Context::new(None);
            let addr = ctx.address();
            let act = f(&mut ctx);
            ctx.set_actor(act);

            let lrx = LocalAddrReceiver::new(0);
            let supervisor = Supervisor {
                ctx: ctx,
                addr: addr,
                sync_msgs: Some(rx),
                unsync_msgs: lrx };
            Arbiter::handle().spawn(supervisor);
            Ok(())
        }));

        Address::new(tx)
    }

    #[inline]
    fn connected(&mut self) -> bool {
        self.unsync_msgs.connected() ||
            self.sync_msgs.as_ref().map(|msgs| msgs.connected()).unwrap_or(false)
    }

    fn remote_address(&mut self) -> Address<A> {
        if self.sync_msgs.is_none() {
            let (tx, rx) = sync::channel(0);
            self.sync_msgs = Some(rx);
            Address::new(tx)
        } else {
            if let Some(ref mut addr) = self.sync_msgs {
                return Address::new(addr.sender())
            }
            unreachable!();
        }
    }

    fn restart(&mut self) {
        let ctx = Context::new(None);
        let ctx = mem::replace(&mut self.ctx, ctx);
        self.ctx.set_actor(ctx.into_inner());
        self.ctx.restarting();
        self.addr = self.ctx.address();
    }
}

#[doc(hidden)]
impl<A> Future for Supervisor<A> where A: Supervised + Actor<Context=Context<A>> {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        'outer: loop {
            // supervisor is not connected, stop supervised context
            if !self.connected() {
                self.ctx.stop();
            }

            let ctx: &mut Context<A> = unsafe{ mem::transmute(&mut self.ctx) };
            let act: &mut A = unsafe{ mem::transmute(ctx.actor()) };

            // poll supervised actor
            match ctx.poll() {
                Ok(Async::NotReady) =>
                    if ctx.waiting() {
                        return Ok(Async::NotReady)
                    },
                Ok(Async::Ready(_)) | Err(_) => {
                    // supervisor is disconnected
                    if !self.connected() {
                        return Ok(Async::Ready(()))
                    }
                    self.restart();
                    continue 'outer;
                }
            }

            let mut not_ready = true;

            // process unsync messages
            loop {
                if !ctx.is_alive() {
                    continue 'outer
                }
                if ctx.waiting() {
                    return Ok(Async::NotReady)
                }

                match self.unsync_msgs.poll() {
                    Ok(Async::Ready(Some(msg))) => {
                        not_ready = false;
                        match msg {
                            LocalAddrProtocol::Upgrade(tx) => {
                                let _ = tx.send(self.remote_address());
                            }
                            LocalAddrProtocol::Envelope(mut env) => {
                                env.env.handle(act, ctx);
                            }
                        }
                    }
                    Ok(Async::NotReady) | Ok(Async::Ready(None)) | Err(_) => break,
                }
            }

            // process sync messages
            if let Some(ref mut msgs) = self.sync_msgs {
                loop {
                    if !ctx.is_alive() {
                        continue 'outer
                    }
                    if ctx.waiting() {
                        return Ok(Async::NotReady)
                    }

                    match msgs.poll() {
                        Ok(Async::Ready(Some(mut env))) => {
                            not_ready = false;
                            env.handle(act, ctx);
                        },
                        Ok(Async::NotReady) | Ok(Async::Ready(None)) | Err(_) => break,
                    }
                }
            }

            if not_ready {
                return Ok(Async::NotReady)
            }
        }
    }
}

trait FnFactory<A: Actor>: 'static where A::Context: AsyncContext<A> {
    fn call(self: Box<Self>, &mut A::Context) -> A;
}

impl<A: Actor, F: FnOnce(&mut A::Context) -> A + 'static> FnFactory<A> for F
    where A::Context: AsyncContext<A>
{
    #[cfg_attr(feature="cargo-clippy", allow(boxed_local))]
    fn call(self: Box<Self>, ctx: &mut A::Context) -> A {
        (*self)(ctx)
    }
}
